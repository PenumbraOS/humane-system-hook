//! `GET /api/settings/schema` — the settings as typed, sectioned fields so a
//! client renders the form instead of hardcoding it. Writes go through the
//! typed `PUT /api/settings`.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::api::ApiState;
use crate::config::{Config, LlmProvider};

pub fn router() -> Router<ApiState> {
    Router::new().route("/api/settings/schema", get(get_schema))
}

async fn get_schema(State(state): State<ApiState>) -> Json<Schema> {
    let config = state.shared_config.read().await;
    Json(describe(&config))
}

#[derive(Serialize)]
struct Schema {
    sections: Vec<Section>,
}

#[derive(Serialize)]
struct Section {
    key: &'static str,
    label: &'static str,
    fields: Vec<Field>,
}

#[derive(Serialize)]
struct Field {
    key: &'static str,
    label: &'static str,
    #[serde(rename = "type")]
    kind: Kind,
    /// Current value. Omitted for `secret` (see `configured`).
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    /// `secret` only: a value is set, without revealing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    configured: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<EnumOption>,
    /// Show only while another field equals a value.
    #[serde(rename = "visibleWhen", skip_serializing_if = "Option::is_none")]
    visible_when: Option<BTreeMap<&'static str, &'static str>>,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Kind {
    String,
    Text,
    Bool,
    Secret,
    Enum,
}

#[derive(Serialize)]
struct EnumOption {
    value: String,
    label: String,
}

/// Keys mirror the config's serde paths (`llm.model`).
fn describe(config: &Config) -> Schema {
    Schema {
        sections: vec![
            Section {
                key: "server",
                label: "Server",
                fields: vec![
                    string("server.display_name", "Display Name",
                        config.server.display_name.clone().unwrap_or_default()),
                    text("server.system_prompt", "System Prompt",
                        config.server.resolved_system_prompt()),
                    text("server.status_prompt", "Status Prompt",
                        config.server.resolved_status_prompt()),
                ],
            },
            Section {
                key: "llm",
                label: "LLM",
                fields: vec![
                    enum_field("llm.provider", "Provider", config.llm.provider.to_string(),
                        provider_options()),
                    string("llm.model", "Model ID", config.llm.model.clone()),
                    secret("llm.api_key", "API Key", config.llm.api_key.is_some()),
                    string("llm.base_url", "Base URL",
                        config.llm.base_url.clone().unwrap_or_default())
                        .visible_when("llm.provider", "openai-compatible"),
                    boolean("llm.web_search", "Provider web search", config.llm.web_search),
                ],
            },
            Section {
                key: "services",
                label: "Services",
                fields: vec![
                    secret("weather.pirate_weather_api_key", "PirateWeather API Key",
                        config.weather.pirate_weather_api_key.is_some()),
                    boolean("contacts.trust_all_contacts", "Trust all contacts",
                        config.contacts.trust_all_contacts),
                    boolean("contacts.allow_all_inbound", "Allow all inbound calls and messages",
                        config.contacts.allow_all_inbound),
                ],
            },
            Section {
                key: "dev",
                label: "Developer",
                fields: vec![boolean("dev.apk_install_enabled", "Remote APK install",
                    config.dev.apk_install_enabled)],
            },
        ],
    }
}

impl Field {
    fn new(key: &'static str, label: &'static str, kind: Kind) -> Field {
        Field { key, label, kind, value: None, configured: None, options: Vec::new(), visible_when: None }
    }

    fn visible_when(mut self, key: &'static str, value: &'static str) -> Field {
        self.visible_when = Some(BTreeMap::from([(key, value)]));
        self
    }
}

fn string(key: &'static str, label: &'static str, value: String) -> Field {
    Field { value: Some(value.into()), ..Field::new(key, label, Kind::String) }
}

fn text(key: &'static str, label: &'static str, value: String) -> Field {
    Field { value: Some(value.into()), ..Field::new(key, label, Kind::Text) }
}

fn boolean(key: &'static str, label: &'static str, value: bool) -> Field {
    Field { value: Some(value.into()), ..Field::new(key, label, Kind::Bool) }
}

/// Write-only: reports only whether a value is set, never the value.
fn secret(key: &'static str, label: &'static str, configured: bool) -> Field {
    Field { configured: Some(configured), ..Field::new(key, label, Kind::Secret) }
}

fn enum_field(key: &'static str, label: &'static str, value: String, options: Vec<EnumOption>) -> Field {
    Field { value: Some(value.into()), options, ..Field::new(key, label, Kind::Enum) }
}

/// The serde variant names of a fieldless enum, read from its JSON schema so a
/// dropdown can't drift from the enum.
fn enum_variants<T: JsonSchema>() -> Vec<String> {
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn provider_options() -> Vec<EnumOption> {
    enum_variants::<LlmProvider>()
        .into_iter()
        .map(|value| EnumOption { label: provider_label(&value), value })
        .collect()
}

/// Friendly label for a provider wire name; unknown/new variants fall back to
/// the wire name so they still appear in the menu.
fn provider_label(value: &str) -> String {
    match value {
        "echo" => "Echo (no API)",
        "gemini" => "Google Gemini",
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "openai-compatible" => "OpenAI-compatible",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A defaults-only config (no file present → serde defaults), like the
    /// config module's own tests.
    fn default_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        Config::load(&dir.path().join("config.toml")).unwrap()
    }

    fn descriptor(config: &Config) -> Value {
        serde_json::to_value(describe(config)).unwrap()
    }

    fn field<'a>(schema: &'a Value, key: &str) -> &'a Value {
        schema["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["fields"].as_array().unwrap())
            .find(|f| f["key"] == key)
            .unwrap_or_else(|| panic!("field {key} not in descriptor"))
    }

    #[test]
    fn secret_fields_report_configured_not_the_value() {
        let mut config = default_config();
        config.llm.api_key = Some("sk-secret".to_string());
        let schema = descriptor(&config);

        let api_key = field(&schema, "llm.api_key");
        assert_eq!(api_key["type"], "secret");
        assert_eq!(api_key["configured"], true);
        // The value must never appear.
        assert!(api_key.get("value").is_none());
        assert!(!schema.to_string().contains("sk-secret"));
    }

    #[test]
    fn enum_and_conditional_fields_are_described() {
        let schema = descriptor(&default_config());

        let provider = field(&schema, "llm.provider");
        assert_eq!(provider["type"], "enum");
        // Options are derived from LlmProvider via schemars, so every variant —
        // including the serde-renamed `openai-compatible` — is present with a label.
        let values: Vec<&str> = provider["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, ["echo", "gemini", "anthropic", "openai", "openai-compatible"]);
        let openai_compatible = provider["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["value"] == "openai-compatible")
            .unwrap();
        assert_eq!(openai_compatible["label"], "OpenAI-compatible");

        // base_url is gated on the openai-compatible provider.
        let base_url = field(&schema, "llm.base_url");
        assert_eq!(base_url["visibleWhen"]["llm.provider"], "openai-compatible");
    }
}
