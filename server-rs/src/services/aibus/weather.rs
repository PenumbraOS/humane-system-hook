use std::sync::Arc;

use prost::Message as _;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::proto::aibus::*;
use crate::proto::common::encryption::{self, EncryptedData};

pub struct WeatherHandler {
    http_client: reqwest::Client,
    pirate_weather_api_key: Arc<RwLock<Option<String>>>,
}

impl WeatherHandler {
    pub fn new(
        http_client: reqwest::Client,
        pirate_weather_api_key: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            http_client,
            pirate_weather_api_key,
        }
    }

    pub async fn encrypted_weather(
        &self,
        request: Request<EncryptedWeatherRequest>,
    ) -> Result<Response<EncryptedWeatherResponse>, Status> {
        let api_key = self.pirate_weather_api_key.read().await;
        let api_key = api_key.as_deref().ok_or_else(|| {
            info!(">>> EncryptedWeather (no API key configured)");
            Status::unavailable(
                "weather not configured — set PIRATE_WEATHER_API_KEY in the environment or .env, or set pirate_weather_api_key in config.toml",
            )
        })?;

        let req = request.into_inner();
        let location_bytes = unwrap_plaintext_data(&req.location)?;
        let location = encryption::LocationEnvelope::decode(location_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad LocationEnvelope: {e}")))?;

        info!(
            lat = location.latitude,
            lon = location.longitude,
            ">>> EncryptedWeather"
        );

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

        let weather = WeatherResponse {
            has_precipitation: precip_intensity > 0.0,
            precipitation_type: precip_type,
            temperature_fahrenheit: temp_f,
            temperature_celsius: temp_c,
            weather_text: summary.clone(),
            weather_icon: pirate_weather_icon_to_device(icon_str),
            u_v_index: uv_index,
        };

        info!(
            temp_f = format!("{temp_f:.0}"),
            temp_c = format!("{temp_c:.0}"),
            summary = %summary,
            icon = %icon_str,
            "<<< EncryptedWeather"
        );

        Ok(Response::new(EncryptedWeatherResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.WeatherResponse",
                weather.encode_to_vec(),
            )),
        }))
    }
}

/// Map PirateWeather icon string to the device's integer weather icon code.
fn pirate_weather_icon_to_device(icon: &str) -> i32 {
    match icon {
        "clear-day" => 1,
        "clear-night" => 33,
        "partly-cloudy-day" => 3,
        "partly-cloudy-night" => 35,
        "cloudy" => 7,
        "rain" => 12,
        "snow" => 19,
        "sleet" => 24,
        "wind" => 32,
        "fog" => 11,
        "thunderstorm" => 15,
        _ => 3,
    }
}
