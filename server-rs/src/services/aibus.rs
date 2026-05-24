use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use prost::Message as _;
use rig::completion::message::Message;
use tokio::sync::RwLock;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::db::Database;
use crate::llm::{LlmAgent, LlmChatRequest, PromptTemplateContext, PromptTemplates};
use crate::nearby::NearbyClient;
use crate::proto::aibus::ai_bus_service_server::AiBusService;
use crate::proto::aibus::*;
use crate::proto::common::encryption;
use crate::synapse::conversation::extract_history;
use crate::synapse::vision::{extract_vision_observation, is_vision_request};

pub struct AiBusServiceImpl {
    pub agent: Arc<RwLock<Arc<LlmAgent>>>,
    pub config: Arc<RwLock<Config>>,
    pub pirate_weather_api_key: Arc<RwLock<Option<String>>>,
    pub http_client: reqwest::Client,
    pub nearby_client: NearbyClient,
    pub db: Database,
}

impl AiBusServiceImpl {
    fn build_prompt_template_context(
        &self,
        req: &SynapseUnderstandingRequest,
        run_id: &str,
        config: &Config,
    ) -> PromptTemplateContext {
        // TODO: Expose specific device fields, like battery level, wifi/cellular status, etc.
        let mut context = PromptTemplateContext::new(run_id, config, chrono::Local::now());

        if let Some(ctx) = req.device_context.as_ref() {
            context.location_name = non_empty_string(&ctx.reverse_geocoded_location);
        }

        if let Some(loc) = req.location.as_ref() {
            let latitude = format_coordinate(loc.latitude);
            let longitude = format_coordinate(loc.longitude);

            context.latitude = Some(latitude.clone());
            context.longitude = Some(longitude.clone());
            context.coordinates = Some(format!("{latitude}, {longitude}"));
        }

        context
    }

    /// Persist a conversation to SQLite in a background task.
    fn spawn_save_conversation(
        &self,
        run_id: &str,
        utterance: &str,
        is_vision: bool,
        history: &[Message],
        response_text: &str,
    ) {
        let db = self.db.clone();
        let run_id = run_id.to_string();
        let utterance = utterance.to_string();
        let history = history.to_vec();
        let response_text = response_text.to_string();

        tokio::spawn(async move {
            if let Err(e) = db
                .save_understand_conversation(
                    &run_id,
                    &utterance,
                    is_vision,
                    &history,
                    &response_text,
                )
                .await
            {
                warn!(error = %e, "failed to save conversation to db");
            }
        });
    }
}

/// Map PirateWeather icon string to the device's integer weather icon code.
fn pirate_weather_icon_to_device(icon: &str) -> i32 {
    match icon {
        "clear-day" => 1,            // weather_01_sunny
        "clear-night" => 33,         // weather_05_clear_skies_night
        "partly-cloudy-day" => 3,    // weather_02_partly_cloudy_day
        "partly-cloudy-night" => 35, // weather_06_partly_cloudy_night
        "cloudy" => 7,               // weather_03_cloudy
        "rain" => 12,                // weather_07_rain
        "snow" => 19,                // weather_10_snow_flurries
        "sleet" => 24,               // weather_12_ice_and_sleet
        "wind" => 32,                // weather_13_windy
        "fog" => 11,                 // weather_14_fog
        "thunderstorm" => 15,        // weather_08_thunderstorms
        _ => 3,                      // safe fallback: partly cloudy day
    }
}

/// Wrap a plaintext proto into an EncryptedData envelope for the bypass hook.
/// `kid` must be the fully-qualified Java class name of the inner proto.
fn wrap_plaintext_envelope(kid: &str, data: Vec<u8>) -> encryption::EncryptedData {
    encryption::EncryptedData {
        encryption_information: Some(encryption::EncryptionInformation { kid: kid.into() }),
        data,
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();

    if !value.is_empty() {
        Some(value.to_string())
    } else {
        None
    }
}

fn format_coordinate(value: f64) -> String {
    format!("{value:.3}")
}

/// Decode the plaintext proto bytes from an EncryptedData envelope.
fn unwrap_plaintext_data(encrypted: &Option<encryption::EncryptedData>) -> Result<&[u8], Status> {
    encrypted
        .as_ref()
        .map(|ed| ed.data.as_slice())
        .ok_or_else(|| Status::invalid_argument("missing encrypted data envelope"))
}

#[tonic::async_trait]
impl AiBusService for AiBusServiceImpl {
    type UnderstandStream =
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>;

    async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<Response<Self::UnderstandStream>, Status> {
        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let utterance = &req.utterance;
        let run_id = metadata
            .get("x-ai-mic-run-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        info!(run_id = %run_id, utterance = %utterance, ">>> Understand");

        // Extract conversation history from device context
        let (history, ctx) = if let Some(ref ctx) = req.device_context {
            info!(
                turns = ctx.turns.len(),
                is_locked = ctx.is_locked,
                location = %ctx.reverse_geocoded_location,
                "    device_context"
            );
            for (i, turn) in ctx.turns.iter().enumerate() {
                let kind = match &turn.content {
                    Some(synapse_chat_turn::Content::UserRequest(_)) => "user_request",
                    Some(synapse_chat_turn::Content::Action(a)) => {
                        debug!(idx = i, action = %a.action, input = %a.input, "    turn");
                        "action"
                    }
                    Some(synapse_chat_turn::Content::Observation(o)) => {
                        debug!(idx = i, is_final = o.is_final, action_name = %o.action_name, obs = %o.observation, "    turn");
                        "observation"
                    }
                    Some(synapse_chat_turn::Content::Message(_)) => "message",
                    Some(synapse_chat_turn::Content::End(_)) => "end",
                    Some(synapse_chat_turn::Content::Tao(_)) => "tao",
                    Some(synapse_chat_turn::Content::Interpretation(_)) => "interpretation",
                    Some(synapse_chat_turn::Content::Speech(_)) => "speech",
                    None => "empty",
                };
                debug!(idx = i, kind = kind, user = ?turn.user(), "    turn");
            }
            let h = extract_history(ctx);
            if !h.is_empty() {
                info!(messages = h.len(), "    extracted history");
            }
            (h, Some(ctx))
        } else {
            (Vec::new(), None)
        };

        if let Some(ref loc) = req.location {
            info!(lat = loc.latitude, lon = loc.longitude, "    location");
        }

        // --- Vision handling ---
        //
        // Phase 1: If this is a fresh vision request (vision_requested == VISION),
        // return an UnderstandScene action to trigger camera capture + AnalyzeImage.
        //
        // Phase 2: If the turns contain a non-final observation for UnderstandScene
        // (the round-trip from AnalyzeImage is complete), echo the observation text
        // back as a Respond action.
        if let Some(ctx) = ctx {
            // Check for phase 2 first: observation already present from AnalyzeImage
            if let Some(observation_text) = extract_vision_observation(ctx) {
                info!(observation = %observation_text, "<<< Vision round-trip complete, responding");

                self.spawn_save_conversation(&run_id, utterance, true, &history, &observation_text);

                // TODO: We probably want to feed the observation + original question
                // back to the LLM for a more conversational/contextualized response,
                // rather than just echoing the raw observation. For now, keep it simple.
                let response = SynapseUnderstandingResponse::action_response(
                    "Respond",
                    "I analyzed the image and should share my observation with the user",
                    &serde_json::json!({"Response": observation_text}).to_string(),
                    &run_id,
                );

                let stream = tokio_stream::once(Ok(response));
                return Ok(Response::new(Box::pin(stream)));
            }

            // Check for phase 1: fresh vision request, no observation yet
            if is_vision_request(ctx) {
                info!("<<< Vision request detected, returning UnderstandScene");

                let response = SynapseUnderstandingResponse::action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    &run_id,
                );

                let stream = tokio_stream::once(Ok(response));
                return Ok(Response::new(Box::pin(stream)));
            }
        }

        // --- Normal (non-vision) handling ---

        // Call LLM agent with conversation history
        let config = self.config.read().await.clone();
        let templates = PromptTemplates {
            system_prompt: config.server.system_prompt.clone(),
            status_prompt: config.server.status_prompt.clone(),
        };
        let template_context = self.build_prompt_template_context(&req, &run_id, &config);
        let chat_request = LlmChatRequest::new(
            utterance.clone(),
            history.clone(),
            templates,
            template_context,
        );
        let agent = self.agent.read().await.clone();
        let response_text = match agent.chat(chat_request).await {
            Ok(text) => text,
            Err(error) => {
                warn!(error = %error, "LLM chat failed, falling back to error message");
                error
            }
        };

        info!(response = %response_text, "<<< Understand responding");

        self.spawn_save_conversation(&run_id, utterance, false, &history, &response_text);

        let response = SynapseUnderstandingResponse::action_response(
            "Respond",
            "I should respond to the user",
            &serde_json::json!({"Response": response_text}).to_string(),
            &run_id,
        );

        // Stream a single response then complete
        let stream = tokio_stream::once(Ok(response));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        let req = request.into_inner();

        let question = if !req.request.is_empty() {
            &req.request
        } else if !req.utterance.is_empty() {
            &req.utterance
        } else {
            "What do you see in this image?"
        };

        let image_bytes = &req.image_data;
        info!(
            question = %question,
            image_bytes = image_bytes.len(),
            hints = ?req.image_hints,
            ">>> AnalyzeImage"
        );

        if image_bytes.is_empty() {
            warn!("AnalyzeImage called with empty image_data");
            return Err(Status::invalid_argument("image_data is empty"));
        }

        // Encode raw JPEG bytes to base64 for the LLM vision API
        let image_b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);

        let agent = self.agent.read().await.clone();
        let observation = match agent.vision_prompt(question, &image_b64).await {
            Ok(text) => text,
            Err(error) => {
                warn!(error = %error, "Vision LLM failed");
                error
            }
        };

        info!(observation = %observation, "<<< AnalyzeImage responding");

        let response = AnalyzeImageResponse {
            observation: String::new(),
            nested_analyze_image_response: Some(NestedAnalyzeImageResponse {
                response_one_of: Some(
                    nested_analyze_image_response::ResponseOneOf::GenericImageResponse(
                        GenericImageResponse { observation },
                    ),
                ),
            }),
        };

        Ok(Response::new(response))
    }

    async fn encrypted_weather(
        &self,
        request: Request<EncryptedWeatherRequest>,
    ) -> Result<Response<EncryptedWeatherResponse>, Status> {
        let api_key = self.pirate_weather_api_key.read().await;
        let api_key = api_key.as_deref().ok_or_else(|| {
            info!(">>> EncryptedWeather (no API key configured)");
            Status::unavailable(
                "weather not configured \u{2014} set PIRATE_WEATHER_API_KEY in the environment or .env, or set pirate_weather_api_key in config.toml",
            )
        })?;

        // Decode LocationEnvelope from the encrypted request
        let req = request.into_inner();
        let location_bytes = unwrap_plaintext_data(&req.location)?;
        let location = encryption::LocationEnvelope::decode(location_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad LocationEnvelope: {e}")))?;

        info!(
            lat = location.latitude,
            lon = location.longitude,
            ">>> EncryptedWeather"
        );

        // Call PirateWeather API (units=us for Fahrenheit)
        let url = format!(
            "https://api.pirateweather.net/forecast/{}/{},{}?units=us&exclude=minutely,hourly,daily,alerts",
            api_key, location.latitude, location.longitude
        );

        let pw_response: serde_json::Value = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                warn!(error = %e, "PirateWeather HTTP request failed");
                Status::unavailable(format!("weather API request failed: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                warn!(error = %e, "PirateWeather response parse failed");
                Status::internal(format!("weather API response parse failed: {e}"))
            })?;

        let currently = pw_response.get("currently").ok_or_else(|| {
            warn!("PirateWeather response missing 'currently' block");
            Status::internal("weather API response missing current conditions")
        })?;

        let temp_f = currently
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let temp_c = (temp_f - 32.0) * 5.0 / 9.0;
        let icon_str = currently
            .get("icon")
            .and_then(|v| v.as_str())
            .unwrap_or("partly-cloudy-day");
        let summary = currently
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let uv_index = currently
            .get("uvIndex")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as i32;
        let precip_intensity = currently
            .get("precipIntensity")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let precip_type = currently
            .get("precipType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let weather_icon = pirate_weather_icon_to_device(icon_str);
        let has_precipitation = precip_intensity > 0.0;

        let weather = WeatherResponse {
            has_precipitation,
            precipitation_type: precip_type,
            temperature_fahrenheit: temp_f,
            temperature_celsius: temp_c,
            weather_text: summary.clone(),
            weather_icon,
            u_v_index: uv_index,
        };

        let response = EncryptedWeatherResponse {
            response: Some(wrap_plaintext_envelope(
                "humane.aibus.WeatherResponse",
                weather.encode_to_vec(),
            )),
        };

        info!(
            temp_f = format!("{temp_f:.0}"),
            temp_c = format!("{temp_c:.0}"),
            summary = %summary,
            icon = %icon_str,
            "<<< EncryptedWeather"
        );
        Ok(Response::new(response))
    }

    async fn encrypted_geo_locate(
        &self,
        _request: Request<EncryptedGeoLocateRequest>,
    ) -> Result<Response<EncryptedGeoLocateResponse>, Status> {
        info!(">>> EncryptedGeoLocate (stub: NOT_FOUND)");

        let geo_response = GeoLocateResponse {
            location: None,
            radius_accuracy: 0.0,
            status: GeoLocateResponseStatus::GeolocateResponseStatusNotFound as i32,
        };

        let response = EncryptedGeoLocateResponse {
            response: Some(wrap_plaintext_envelope(
                "humane.aibus.GeoLocateResponse",
                geo_response.encode_to_vec(),
            )),
        };

        Ok(Response::new(response))
    }

    async fn encrypted_nearby_search(
        &self,
        request: Request<EncryptedNearbySearchRequest>,
    ) -> Result<Response<EncryptedNearbySearchResponse>, Status> {
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let nearby_req = NearbySearchRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad NearbySearchRequest: {e}")))?;

        let location = nearby_req
            .location
            .ok_or_else(|| Status::invalid_argument("NearbySearchRequest missing location"))?;
        let lat = location.latitude;
        let lon = location.longitude;
        let radius = if nearby_req.radius_accuracy > 0.0 {
            nearby_req.radius_accuracy
        } else {
            1000.0
        };

        info!(
            lat = lat,
            lon = lon,
            radius = radius,
            query = %nearby_req.text_query,
            ">>> EncryptedNearbySearch"
        );

        let nearby_places = self
            .nearby_client
            .search(lat, lon, radius, &nearby_req.text_query)
            .await?;

        let result_count = nearby_places.len();

        let nearby_response = NearbySearchResponse {
            nearby_places,
            status: Some(NearbySearchResultStatus::Success as i32),
        };

        let response = EncryptedNearbySearchResponse {
            response: Some(wrap_plaintext_envelope(
                "humane.aibus.NearbySearchResponse",
                nearby_response.encode_to_vec(),
            )),
        };

        info!(results = result_count, "<<< EncryptedNearbySearch");
        Ok(Response::new(response))
    }
}
