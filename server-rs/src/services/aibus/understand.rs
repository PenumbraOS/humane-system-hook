use std::pin::Pin;

use futures::StreamExt;
use prost::Message as _;
use rig::completion::message::Message;
use tokio_stream::Stream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

use std::sync::Arc;

use super::envelope::unwrap_plaintext_data;
use crate::config::ResolvedConfig;
use crate::db::Database;
use crate::llm::ChatResult;
use crate::llm::memory::MemoryService;
use crate::llm::{LlmAgent, LlmChatRequest, PromptTemplateContext, PromptTemplates};
use crate::music::{MusicProvider, NowPlayingHandle};
use crate::proto::aibus::*;
use crate::proto::common::encryption::{self, EncryptedData};
use crate::synapse::conversation::extract_history;
use crate::synapse::extract_run_id;
use crate::synapse::image_store::LiveImageStore;
use crate::synapse::vision::{extract_most_recent_image_data, is_vision_request};

pub struct UnderstandHandler {
    agent: Arc<LlmAgent>,
    config: Arc<ResolvedConfig>,
    db: Database,
    memory: Option<MemoryService>,
    image_store: LiveImageStore,
    music_provider: Arc<MusicProvider>,
    now_playing: NowPlayingHandle,
}

impl UnderstandHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: Arc<LlmAgent>,
        config: Arc<ResolvedConfig>,
        db: Database,
        memory: Option<MemoryService>,
        image_store: LiveImageStore,
        music_provider: Arc<MusicProvider>,
        now_playing: NowPlayingHandle,
    ) -> Self {
        Self {
            agent,
            config,
            db,
            memory,
            image_store,
            music_provider,
            now_playing,
        }
    }

    async fn build_prompt_template_context(
        &self,
        req: &SynapseUnderstandingRequest,
        run_id: &str,
        config: &ResolvedConfig,
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

        // Current song, so the assistant can answer "what's playing" etc.
        context.now_playing = self.now_playing.read().await.as_ref().map(|np| np.summary());

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

    /// Call a configured agent with the given conversation context
    async fn evaluate_agent_conversation(
        &self,
        req: &SynapseUnderstandingRequest,
        run_id: &str,
        utterance: &str,
        history: &[Message],
        image: Option<Vec<u8>>,
        log_name: &str,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>,
        Status,
    > {
        let does_have_image = image.is_some();

        let templates = PromptTemplates {
            system_prompt: self.config.config.server.resolved_system_prompt(),
            status_prompt: self.config.config.server.resolved_status_prompt(),
        };

        let template_context = self
            .build_prompt_template_context(req, run_id, &self.config)
            .await;
        let memory_context = if let Some(memory) = &self.memory {
            match memory.retrieve_context(utterance.to_string()).await {
                Ok(context) => context,
                Err(error) => {
                    warn!(error = %error, "memory retrieval failed");
                    None
                }
            }
        } else {
            None
        };

        let mut chat_request = LlmChatRequest::new(
            utterance.to_string(),
            history.to_vec(),
            templates,
            template_context,
            memory_context,
        );

        if let Some(image_bytes) = image {
            chat_request = chat_request.with_image(image_bytes);
        }

        match self.agent.chat(chat_request).await {
            Ok(ChatResult::Text(response_text)) => {
                info!(response = %response_text, "<<< {log_name} responding");
                self.spawn_save_conversation(
                    run_id,
                    utterance,
                    does_have_image,
                    history,
                    &response_text,
                );
                let response = SynapseUnderstandingResponse::action_response(
                    "Respond",
                    "I should respond to the user",
                    &serde_json::json!({"Response": response_text}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Ok(ChatResult::DeferredVision) => {
                info!("<<< LLM requested vision, returning UnderstandScene");
                let response = SynapseUnderstandingResponse::action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Ok(ChatResult::PlayMusic(args_json)) => {
                let input =
                    build_play_music_input(&self.music_provider, &args_json, utterance).await;
                // If a specifically-named song resolves to an Apple-unplayable
                // (>2^31 id) track, speak an alert instead of silently stalling.
                if let Some(message) =
                    apple_unplayable_message(&self.music_provider, &input).await
                {
                    info!(message = %message, "<<< specific song unplayable on Apple, alerting");
                    let mut response = SynapseUnderstandingResponse::action_response(
                        "Respond",
                        "The requested song can't play on the current provider",
                        &serde_json::json!({ "Response": message }).to_string(),
                        run_id,
                    );
                    // Populate the top-level spoken response + finalize so the Pin
                    // reads the alert aloud, not just displays it.
                    response.response = message.clone();
                    response.is_final = true;
                    return Ok(Box::pin(tokio_stream::once(Ok(response))));
                }
                info!(input = %input, "<<< LLM requested music playback, returning PlayMusic");
                let response = SynapseUnderstandingResponse::action_response(
                    "PlayMusic",
                    "The user wants to play music",
                    &input,
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Ok(ChatResult::MusicControl(action)) => {
                // The device's `PlayFavoriteTracks` action needs a logged-in
                // Tidal user session we bypass, so it silently no-ops. Route it
                // through PlayMusic with a sentinel term instead, which the shim
                // resolves to the user's real library (a playable queue).
                if action == "PlayFavoriteTracks" {
                    let input = serde_json::json!({
                        "Track": crate::services::tidal_shim::FAVORITES_TERM
                    })
                    .to_string();
                    info!("<<< play favorites -> PlayMusic(library queue)");
                    let response = SynapseUnderstandingResponse::action_response(
                        "PlayMusic",
                        "The user wants to play their favorite songs",
                        &input,
                        run_id,
                    );
                    return Ok(Box::pin(tokio_stream::once(Ok(response))));
                }
                info!(action = %action, "<<< LLM requested music transport control");
                // These device transport actions take no input fields.
                let response = SynapseUnderstandingResponse::action_response(
                    &action,
                    "The user wants to control music playback",
                    "{}",
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Err(error) => {
                warn!(error = %error, "LLM chat failed, falling back to error message");
                self.spawn_save_conversation(run_id, utterance, does_have_image, history, &error);
                let response = SynapseUnderstandingResponse::action_response(
                    "Respond",
                    "I encountered an error",
                    &serde_json::json!({"Response": error}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
        }
    }

    async fn understand_inner(
        &self,
        metadata: MetadataMap,
        req: SynapseUnderstandingRequest,
        log_name: &str,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>,
        Status,
    > {
        let utterance = &req.utterance;
        let run_id = extract_run_id(&metadata);

        info!(run_id = %run_id, utterance = %utterance, ">>> {log_name}");

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
            let h = extract_history(ctx, &self.image_store).await;
            if !h.is_empty() {
                info!(messages = h.len(), "    extracted history");
            }
            (h, Some(ctx))
        } else {
            (Vec::new(), None)
        };

        if let Some(ctx) = &ctx {
            // image_data attached inline by a device hook
            // This is only accessible if our modified hook code does this
            if let Some(image_bytes) = extract_most_recent_image_data(ctx) {
                info!(
                    image_bytes = image_bytes.len(),
                    "<<< Inline image data in Understand request, running chat with image"
                );
                return self
                    .evaluate_agent_conversation(
                        &req,
                        &run_id,
                        utterance,
                        &history,
                        Some(image_bytes),
                        log_name,
                    )
                    .await;
            }

            // A previous turn called AnalyzeImage and stored an image for us to retrieve in this step
            if let Some(image_bytes) = self.image_store.get_refresh(&run_id).await {
                info!(run_id = %run_id, image_bytes = image_bytes.len(), "<<< Have stored image for current run, running chat with image");
                return self
                    .evaluate_agent_conversation(
                        &req,
                        &run_id,
                        utterance,
                        &history,
                        Some(image_bytes),
                        log_name,
                    )
                    .await;
            }

            // Explicit vision request
            if is_vision_request(ctx) {
                info!("<<< Vision request detected, returning UnderstandScene");

                let response = SynapseUnderstandingResponse::action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    &run_id,
                );

                return Ok(Box::pin(tokio_stream::once(Ok(response))));
            }
        }

        // No images found in context, do a normal chat
        self.evaluate_agent_conversation(&req, &run_id, utterance, &history, None, log_name)
            .await
    }

    pub async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>>,
        Status,
    > {
        let metadata = request.metadata().clone();
        let req = request.into_inner();
        let stream = self.understand_inner(metadata, req, "Understand").await?;
        Ok(Response::new(stream))
    }

    pub async fn encrypted_understand(
        &self,
        request: Request<EncryptedSynapseUnderstandingRequest>,
    ) -> Result<
        Response<
            Pin<
                Box<
                    dyn Stream<Item = Result<EncryptedSynapseUnderstandingResponse, Status>> + Send,
                >,
            >,
        >,
        Status,
    > {
        let metadata = request.metadata().clone();
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let mut plain_req = SynapseUnderstandingRequest::decode(request_bytes).map_err(|e| {
            Status::invalid_argument(format!("bad SynapseUnderstandingRequest: {e}"))
        })?;

        if let Some(location_envelope) = req.location.as_ref() {
            if !location_envelope.data.is_empty() {
                let location =
                    encryption::LocationEnvelope::decode(location_envelope.data.as_slice())
                        .map_err(|e| {
                            Status::invalid_argument(format!("bad LocationEnvelope: {e}"))
                        })?;
                plain_req.location = Some(Location {
                    latitude: location.latitude as f64,
                    longitude: location.longitude as f64,
                });
            }
        }

        let plain_stream = self
            .understand_inner(metadata, plain_req, "EncryptedUnderstand")
            .await?;
        let encrypted_stream = plain_stream.map(|item| {
            item.map(|plain_response| EncryptedSynapseUnderstandingResponse {
                response: Some(EncryptedData::new(
                    "humane.aibus.SynapseUnderstandingResponse",
                    plain_response.encode_to_vec(),
                )),
            })
        });

        Ok(Response::new(Box::pin(encrypted_stream)))
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

/// When a specifically-named song (the `Track` field) resolves to an
/// Apple-unplayable track (id >= 2^31 — the 2021 SDK's 32-bit overflow), return a
/// spoken alert. Only checks the single-song case to keep latency low; queues get
/// the unplayable tracks filtered out in the shim instead.
async fn apple_unplayable_message(provider: &MusicProvider, input_json: &str) -> Option<String> {
    if !matches!(provider, MusicProvider::Apple(_)) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(input_json).ok()?;
    let track = value.get("Track")?.as_str()?;
    // The device searches the combined "Track Artist" phrase, so match that here
    // or the resolved id (and thus the playable check) won't line up.
    let phrase = match value.get("Artist").and_then(|a| a.as_str()) {
        Some(artist) if !artist.is_empty() => format!("{track} {artist}"),
        _ => track.to_string(),
    };
    let top = provider.search_top(&phrase).await.ok()??;
    if crate::music::apple_id_playable(&top.id) {
        return None;
    }
    Some(format!(
        "Sorry — \"{}\" is a recent release that can't play on Apple Music right now due to a known \
         limitation with newer songs. You can switch to Spotify or YouTube in settings to play it.",
        top.title
    ))
}

/// Map the `play_music` tool arguments onto the device `PlayMusicAction` input
/// fields, doing the resolution the device can't:
/// - `mood` (genre/vibe/activity) -> `Playlist` so the device runs a playlist
///   search the shim answers with a matching Apple playlist (the device's own
///   genre resolver only knows ~8 genres and often fails).
/// - `latest_album` -> resolve the artist's newest album name server-side and
///   hand the device that concrete album.
/// - track/artist/album pass through to their resolver paths.
async fn build_play_music_input(
    provider: &MusicProvider,
    args_json: &str,
    utterance: &str,
) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(args_json).unwrap_or_else(|_| serde_json::json!({}));
    let field = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(non_empty_string)
    };
    let mut input = serde_json::Map::new();

    let artist = field("artist");
    let wants_latest = parsed
        .get("latest_album")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Artist's newest album: resolve the concrete album title now.
    if wants_latest {
        if let Some(ref artist) = artist {
            if let Ok(Some(album)) = provider.latest_album(artist).await {
                info!(artist = %artist, album = %album, "resolved latest album");
                input.insert("Album".to_string(), serde_json::json!(album));
                input.insert("Artist".to_string(), serde_json::json!(artist));
            }
        }
    }

    // The user's OWN saved/library playlist -> a personal-library sentinel the
    // shim resolves against the signed-in account (the device's library-playlist
    // action needs a Tidal user id we don't have, so it no-ops). "my playlists"
    // (no specific name) plays a shuffled mix across them.
    if input.is_empty() {
        if let Some(playlist) = field("playlist") {
            let p = playlist.to_lowercase();
            let term = if matches!(
                p.as_str(),
                "my playlists" | "all" | "my library" | "everything" | "my playlist"
            ) {
                crate::services::tidal_shim::MY_PLAYLISTS_TERM.to_string()
            } else {
                format!("{}{}", crate::services::tidal_shim::PLAYLIST_PREFIX, playlist)
            };
            input.insert("Track".to_string(), serde_json::json!(term));
        }
    }

    // Mood/genre/vibe -> the device's Album path with a `playlist:` marker. The
    // shim resolves that to a matching Apple playlist and serves its tracks. (The
    // device's own Genre path only knows ~8 genres, and its Playlist path needs a
    // Tidal user id we don't have — both fail before reaching the shim.)
    if input.is_empty() {
        if let Some(mood) = field("mood") {
            input.insert("Album".to_string(), serde_json::json!(format!("playlist:{mood}")));
        }
    }

    // Specific track/artist/album pass through to their resolver paths.
    if input.is_empty() {
        for (arg_key, field_name) in [("track", "Track"), ("artist", "Artist"), ("album", "Album")] {
            if let Some(value) = field(arg_key) {
                input.insert(field_name.to_string(), serde_json::json!(value));
            }
        }
    }

    if input.is_empty() {
        input.insert("Query".to_string(), serde_json::json!(utterance));
    }

    serde_json::Value::Object(input).to_string()
}
