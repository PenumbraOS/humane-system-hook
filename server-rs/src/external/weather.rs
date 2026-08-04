//! Shared PirateWeather service client helpers.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

const PIRATE_WEATHER_FORECAST_URL: &str = "https://api.pirateweather.net/forecast";
const DEFAULT_HOURLY_LIMIT: usize = 12;
const DEFAULT_DAILY_LIMIT: usize = 7;
const MAX_HOURLY_LIMIT: usize = 48;
const MAX_DAILY_LIMIT: usize = 14;

/// PirateWeather API client.
#[derive(Clone)]
pub struct WeatherClient {
    http: reqwest::Client,
    api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CurrentWeather {
    pub temperature_fahrenheit: f64,
    pub temperature_celsius: f64,
    pub summary: String,
    pub icon: String,
    pub uv_index: i32,
    pub has_precipitation: bool,
    pub precipitation_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WeatherRequest {
    pub latitude: f64,
    pub longitude: f64,
    /// Optional ISO 8601 date/time for time-machine or date-specific requests.
    pub time: Option<String>,
    pub include_current: bool,
    pub include_hourly: bool,
    pub include_daily: bool,
    pub include_alerts: bool,
    pub hourly_limit: usize,
    pub daily_limit: usize,
}

impl WeatherRequest {
    pub fn current(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude,
            longitude,
            time: None,
            include_current: true,
            include_hourly: false,
            include_daily: false,
            include_alerts: false,
            hourly_limit: 0,
            daily_limit: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WeatherReport {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<WeatherPoint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hourly: Vec<WeatherPoint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub daily: Vec<WeatherPoint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<WeatherAlertSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WeatherPoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_fahrenheit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_celsius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apparent_temperature_fahrenheit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apparent_temperature_celsius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_high_fahrenheit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_low_fahrenheit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precipitation_probability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precipitation_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precipitation_intensity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humidity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_index: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WeatherAlertSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug)]
pub enum WeatherError {
    NotConfigured,
    InvalidTime(String),
    UnsupportedRequest(String),
    HttpRequest(reqwest::Error),
    ParseResponse(reqwest::Error),
    MissingCurrentConditions,
}

impl std::fmt::Display for WeatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("weather not configured"),
            Self::InvalidTime(value) => write!(f, "invalid ISO 8601 weather time: {value}"),
            Self::UnsupportedRequest(message) => f.write_str(message),
            Self::HttpRequest(e) => write!(f, "PirateWeather HTTP request failed: {e}"),
            Self::ParseResponse(e) => write!(f, "PirateWeather response parse failed: {e}"),
            Self::MissingCurrentConditions => {
                f.write_str("PirateWeather response missing current conditions")
            }
        }
    }
}

impl std::error::Error for WeatherError {}

impl WeatherClient {
    pub fn new(http: reqwest::Client, api_key: Option<String>) -> Self {
        Self { http, api_key }
    }

    pub fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    pub async fn current(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> Result<CurrentWeather, WeatherError> {
        let report = self.weather(WeatherRequest::current(latitude, longitude)).await?;
        let currently = report.current.ok_or(WeatherError::MissingCurrentConditions)?;
        let temperature_fahrenheit = currently.temperature_fahrenheit.unwrap_or(0.0);
        let temperature_celsius = currently
            .temperature_celsius
            .unwrap_or_else(|| fahrenheit_to_celsius(temperature_fahrenheit));
        let precipitation_intensity = currently.precipitation_intensity.unwrap_or(0.0);

        Ok(CurrentWeather {
            temperature_fahrenheit,
            temperature_celsius,
            summary: currently.summary.unwrap_or_default(),
            icon: currently
                .icon
                .unwrap_or_else(|| "partly-cloudy-day".to_string()),
            uv_index: currently.uv_index.unwrap_or(0),
            has_precipitation: precipitation_intensity > 0.0,
            precipitation_type: currently.precipitation_type,
        })
    }

    pub async fn weather(&self, request: WeatherRequest) -> Result<WeatherReport, WeatherError> {
        let api_key = self.api_key.clone().ok_or(WeatherError::NotConfigured)?;
        let url = build_weather_url(&api_key, &request)?;

        // Strip URLS from errors to avoid leaking the API key
        let response: serde_json::Value = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| WeatherError::HttpRequest(e.without_url()))?
            .error_for_status()
            .map_err(|e| WeatherError::HttpRequest(e.without_url()))?
            .json()
            .await
            .map_err(|e| WeatherError::ParseResponse(e.without_url()))?;

        Ok(parse_weather_report(&response, &request))
    }
}

fn build_weather_url(api_key: &str, request: &WeatherRequest) -> Result<String, WeatherError> {
    let location = match request.time.as_deref() {
        Some(time) => format!(
            "{},{},{}",
            request.latitude,
            request.longitude,
            parse_iso8601_to_unix(time)?
        ),
        None => format!("{},{}", request.latitude, request.longitude),
    };

    let mut excluded = vec!["minutely"];
    if !request.include_current {
        excluded.push("currently");
    }
    if !request.include_hourly {
        excluded.push("hourly");
    }
    if !request.include_daily {
        excluded.push("daily");
    }
    if !request.include_alerts {
        excluded.push("alerts");
    }

    Ok(format!(
        "{PIRATE_WEATHER_FORECAST_URL}/{api_key}/{location}?units=us&exclude={}",
        excluded.join(",")
    ))
}

pub(crate) fn parse_iso8601_to_unix(value: &str) -> Result<i64, WeatherError> {
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Ok(datetime.timestamp());
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let datetime = date
            .and_hms_opt(12, 0, 0)
            .ok_or_else(|| WeatherError::InvalidTime(value.to_string()))?;
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc).timestamp());
    }

    Err(WeatherError::InvalidTime(value.to_string()))
}

fn parse_weather_report(value: &serde_json::Value, request: &WeatherRequest) -> WeatherReport {
    let current = value.get("currently").map(parse_weather_point);
    let hourly_limit = request
        .hourly_limit
        .min(MAX_HOURLY_LIMIT)
        .max(if request.include_hourly { 1 } else { 0 });
    let daily_limit = request
        .daily_limit
        .min(MAX_DAILY_LIMIT)
        .max(if request.include_daily { 1 } else { 0 });

    let hourly = value
        .get("hourly")
        .and_then(|block| block.get("data"))
        .and_then(|data| data.as_array())
        .into_iter()
        .flatten()
        .take(hourly_limit)
        .map(parse_weather_point)
        .collect();

    let daily = value
        .get("daily")
        .and_then(|block| block.get("data"))
        .and_then(|data| data.as_array())
        .into_iter()
        .flatten()
        .take(daily_limit)
        .map(parse_weather_point)
        .collect();

    let alerts = value
        .get("alerts")
        .and_then(|alerts| alerts.as_array())
        .into_iter()
        .flatten()
        .map(parse_alert)
        .collect();

    WeatherReport {
        latitude: value
            .get("latitude")
            .and_then(|v| v.as_f64())
            .unwrap_or(request.latitude),
        longitude: value
            .get("longitude")
            .and_then(|v| v.as_f64())
            .unwrap_or(request.longitude),
        timezone: optional_string(value, "timezone"),
        current,
        hourly,
        daily,
        alerts,
    }
}

fn parse_weather_point(value: &serde_json::Value) -> WeatherPoint {
    let temperature_fahrenheit = optional_f64(value, "temperature");
    let apparent_temperature_fahrenheit = optional_f64(value, "apparentTemperature");

    WeatherPoint {
        time: value.get("time").and_then(|v| v.as_i64()),
        summary: optional_string(value, "summary"),
        icon: optional_string(value, "icon"),
        temperature_fahrenheit,
        temperature_celsius: temperature_fahrenheit.map(fahrenheit_to_celsius),
        apparent_temperature_fahrenheit,
        apparent_temperature_celsius: apparent_temperature_fahrenheit.map(fahrenheit_to_celsius),
        temperature_high_fahrenheit: optional_f64(value, "temperatureHigh")
            .or_else(|| optional_f64(value, "temperatureMax")),
        temperature_low_fahrenheit: optional_f64(value, "temperatureLow")
            .or_else(|| optional_f64(value, "temperatureMin")),
        precipitation_probability: optional_f64(value, "precipProbability"),
        precipitation_type: optional_string(value, "precipType"),
        precipitation_intensity: optional_f64(value, "precipIntensity"),
        humidity: optional_f64(value, "humidity"),
        wind_speed: optional_f64(value, "windSpeed"),
        uv_index: optional_i32(value, "uvIndex"),
    }
}

fn parse_alert(value: &serde_json::Value) -> WeatherAlertSummary {
    WeatherAlertSummary {
        title: optional_string(value, "title"),
        severity: optional_string(value, "severity"),
        time: value.get("time").and_then(|v| v.as_i64()),
        expires: value.get("expires").and_then(|v| v.as_i64()),
        description: optional_string(value, "description"),
        uri: optional_string(value, "uri"),
    }
}

fn optional_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn optional_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|v| v.as_f64())
}

fn optional_i32(value: &serde_json::Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(|v| v.as_i64())
        .and_then(|value| i32::try_from(value).ok())
        .or_else(|| value.get(key).and_then(|v| v.as_f64()).map(|value| value as i32))
}

fn fahrenheit_to_celsius(value: f64) -> f64 {
    (value - 32.0) * 5.0 / 9.0
}

impl Default for WeatherRequest {
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            time: None,
            include_current: true,
            include_hourly: true,
            include_daily: true,
            include_alerts: false,
            hourly_limit: DEFAULT_HOURLY_LIMIT,
            daily_limit: DEFAULT_DAILY_LIMIT,
        }
    }
}
