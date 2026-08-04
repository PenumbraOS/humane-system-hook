use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

/// Top-level configuration, loaded from `config.toml`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub weather: WeatherConfig,
    #[serde(default)]
    pub contacts: ContactsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub dev: DevConfig,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub config: Config,
    pub pirate_weather_api_key: Option<String>,
}

impl ResolvedConfig {
    pub fn resolve(config: Config) -> Self {
        let pirate_weather_api_key = config.weather.resolve_api_key();

        Self {
            config,
            pirate_weather_api_key,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    Echo,
    Gemini,
    Anthropic,
    OpenAi,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

impl LlmProvider {
    // TODO: This may be removable based on Serde's serializer
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LlmConfig {
    /// Provider name: "gemini", "anthropic", "openai", "openai-compatible", "echo"
    #[serde(default = "default_provider")]
    pub provider: LlmProvider,

    /// Model ID for the chosen provider (e.g. "gemini-2.5-flash")
    #[serde(default = "default_model")]
    pub model: String,

    /// API key — overrides the corresponding env var if set.
    pub api_key: Option<String>,

    /// Base URL — only used for "openai-compatible" provider.
    pub base_url: Option<String>,

    /// Enable provider-hosted web search grounding (currently only Gemini and OpenAI)
    #[serde(default)]
    pub web_search: bool,

    /// Max output tokens per completion.
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u64,

    /// Server-local native LLM tools.
    #[serde(default)]
    pub tools: LlmToolsConfig,

    /// Long-term assistant memory.
    #[serde(default)]
    pub memory: LlmMemoryConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct LlmMemoryConfig {
    /// Enable long-term assistant memory.
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,

    /// Path to the Memvid .mv2 memory file.
    #[serde(default = "default_memory_path")]
    pub path: String,

    /// Number of memories to retrieve for each request.
    #[serde(default = "default_memory_top_k")]
    pub top_k: usize,

    /// Number of characters to include per retrieved snippet.
    #[serde(default = "default_memory_snippet_chars")]
    pub snippet_chars: usize,

    /// Maximum total characters injected into the prompt.
    #[serde(default = "default_memory_max_context_chars")]
    pub max_context_chars: usize,

    /// Automatically retrieve relevant memory before LLM calls.
    #[serde(default = "default_memory_auto_retrieve")]
    pub auto_retrieve: bool,

    /// Automatically save conversation turns. Kept disabled initially; writes are explicit via tools.
    #[serde(default)]
    pub auto_remember: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct LlmToolsConfig {
    /// Enable server-local native tool calling.
    #[serde(default = "default_llm_tools_enabled")]
    pub enabled: bool,

    /// Number of dynamically retrieved tool schemas to send to the model.
    #[serde(default = "default_dynamic_tool_count")]
    pub dynamic_tool_count: usize,

    /// Maximum model/tool loop turns per request.
    #[serde(default = "default_max_tool_turns")]
    pub max_tool_turns: usize,

    /// Maximum concurrent tool calls per tool turn.
    #[serde(default = "default_tool_concurrency")]
    pub tool_concurrency: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    /// HTTP listen address for uploads and REST API.
    #[serde(default = "default_http_bind_addr")]
    pub http_bind_addr: String,

    /// gRPC listen address for on-device RPCs.
    #[serde(default = "default_grpc_bind_addr")]
    pub grpc_bind_addr: String,

    /// Public address the device will use to reach this server (e.g. "127.0.0.1:8080").
    /// Used for constructing upload URLs.
    #[serde(default = "default_public_addr")]
    pub public_addr: String,

    /// System prompt template sent to the LLM.
    ///
    /// `None` means use the current built-in default prompt. A concrete value
    /// is treated as user-customized and must not be empty.
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Request status prompt template sent to the LLM after app-provided history.
    ///
    /// `None` means use the current built-in default prompt. A concrete value
    /// is treated as user-customized and must not be empty.
    #[serde(default)]
    pub status_prompt: Option<String>,

    /// Display name shown during onboarding welcome screen.
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StorageConfig {
    /// Directory for storing captured media files.
    #[serde(default = "default_media_dir")]
    pub media_dir: String,

    /// Path to the SQLite database file.
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WeatherConfig {
    /// PirateWeather API key. If not set, weather requests return "unavailable".
    pub pirate_weather_api_key: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ContactsConfig {
    /// Treat all contacts/numbers as trusted at runtime.
    #[serde(default)]
    pub trust_all_contacts: bool,

    /// Allow all inbound calls/messages without requiring contact lookup.
    #[serde(default)]
    pub allow_all_inbound: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LoggingConfig {
    /// Directory where rolling log files are written. If empty/None, no file
    /// appender is installed and `/api/logs/server` will return 503.
    pub log_dir: Option<String>,

    /// File-name prefix for rolled log files (suffix is `YYYY-MM-DD`).
    #[serde(default = "default_log_file_prefix")]
    pub file_prefix: String,

    /// How many rolled files to retain on disk.
    #[serde(default = "default_log_max_files")]
    pub max_files: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct DevConfig {
    /// Enable remote APK installs.
    #[serde(default)]
    pub apk_install_enabled: bool,
}

fn default_log_file_prefix() -> String {
    "humane-server".into()
}

fn default_log_max_files() -> usize {
    7
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            log_dir: None,
            file_prefix: default_log_file_prefix(),
            max_files: default_log_max_files(),
        }
    }
}

// --- defaults ---

fn default_provider() -> LlmProvider {
    LlmProvider::Echo
}

fn default_max_output_tokens() -> u64 {
    32_000
}

fn default_model() -> String {
    "gemini-2.5-flash".into()
}

fn default_llm_tools_enabled() -> bool {
    true
}

fn default_dynamic_tool_count() -> usize {
    8
}

fn default_max_tool_turns() -> usize {
    5
}

fn default_tool_concurrency() -> usize {
    2
}

fn default_memory_enabled() -> bool {
    true
}

fn default_memory_path() -> String {
    "./data/assistant-memory.mv2".into()
}

fn default_memory_top_k() -> usize {
    5
}

fn default_memory_snippet_chars() -> usize {
    500
}

fn default_memory_max_context_chars() -> usize {
    1500
}

fn default_memory_auto_retrieve() -> bool {
    true
}

fn default_http_bind_addr() -> String {
    "0.0.0.0:8080".into()
}

fn default_grpc_bind_addr() -> String {
    "127.0.0.1:9090".into()
}

fn default_public_addr() -> String {
    "127.0.0.1:8080".into()
}

fn default_system_prompt() -> String {
    "You are a helpful assistant running on a Humane AI Pin. Keep responses concise - they will be displayed on a laser projector and spoken aloud.".into()
}

fn default_status_prompt() -> String {
    r#"Current request status:
- Current timestamp: {{current_timestamp}}
- Current date: {{current_date}}
- Current time: {{current_time}}
{{#if location_name}}- User location: {{location_name}}{{else}}- User location: unknown
{{/if}}{{#if coordinates}}- User coordinates: {{coordinates}}
{{/if}}
This status applies to the current user request only. If it conflicts with earlier conversation history, prefer this current status."#.into()
}

fn default_media_dir() -> String {
    "./media".into()
}

fn default_db_path() -> String {
    "./data/penumbra.db".into()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            api_key: None,
            base_url: None,
            web_search: false,
            max_output_tokens: default_max_output_tokens(),
            tools: LlmToolsConfig::default(),
            memory: LlmMemoryConfig::default(),
        }
    }
}

impl Default for LlmMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            path: default_memory_path(),
            top_k: default_memory_top_k(),
            snippet_chars: default_memory_snippet_chars(),
            max_context_chars: default_memory_max_context_chars(),
            auto_retrieve: default_memory_auto_retrieve(),
            auto_remember: false,
        }
    }
}

impl Default for LlmToolsConfig {
    fn default() -> Self {
        Self {
            enabled: default_llm_tools_enabled(),
            dynamic_tool_count: default_dynamic_tool_count(),
            max_tool_turns: default_max_tool_turns(),
            tool_concurrency: default_tool_concurrency(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_bind_addr: default_http_bind_addr(),
            grpc_bind_addr: default_grpc_bind_addr(),
            public_addr: default_public_addr(),
            system_prompt: None,
            status_prompt: None,
            display_name: None,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            media_dir: default_media_dir(),
            db_path: default_db_path(),
        }
    }
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            pirate_weather_api_key: None,
        }
    }
}

impl Config {
    /// Load config from file. Falls back to defaults if file is missing.
    /// If a sibling `config.local.toml` exists, it is recursively merged over
    /// the selected config file for local development overrides.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config_value = if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let value: toml::Value = toml::from_str(&contents)?;
            info!(?path, "loaded config");
            value
        } else {
            info!(?path, "config file not found, using defaults");
            toml::Value::Table(toml::map::Map::new())
        };

        let local_path = local_config_path(path);
        if local_path.exists() {
            let contents = std::fs::read_to_string(&local_path)?;
            let local_value: toml::Value = toml::from_str(&contents)?;
            merge_toml(&mut config_value, local_value);
            info!(path = ?local_path, base_path = ?path, "loaded local config override");
        }

        let mut config: Config = config_value.try_into()?;
        config.server.normalize()?;

        #[cfg(target_os = "android")]
        {
            if !std::path::Path::new(&config.storage.media_dir).is_absolute() {
                return Err(format!(
                    "Android requires absolute storage.media_dir, got {}",
                    config.storage.media_dir
                )
                .into());
            }
            if !std::path::Path::new(&config.storage.db_path).is_absolute() {
                return Err(format!(
                    "Android requires absolute storage.db_path, got {}",
                    config.storage.db_path
                )
                .into());
            }
            if config.llm.memory.enabled
                && !std::path::Path::new(&config.llm.memory.path).is_absolute()
            {
                return Err(format!(
                    "Android requires absolute llm.memory.path when memory is enabled, got {}",
                    config.llm.memory.path
                )
                .into());
            }
        }

        Ok(config)
    }
}

fn local_config_path(path: &Path) -> PathBuf {
    path.parent()
        .map(|parent| parent.join("config.local.toml"))
        .unwrap_or_else(|| PathBuf::from("config.local.toml"))
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, overlay_value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(base_value) => merge_toml(base_value, overlay_value),
                    None => {
                        base_table.insert(key, overlay_value);
                    }
                }
            }
        }
        (base_value, overlay_value) => {
            *base_value = overlay_value;
        }
    }
}

impl ServerConfig {
    /// Resolve the configured system prompt, falling back to the current built-in default.
    pub fn resolved_system_prompt(&self) -> String {
        self.system_prompt
            .clone()
            .unwrap_or_else(default_system_prompt)
    }

    /// Resolve the configured status prompt, falling back to the current built-in default.
    pub fn resolved_status_prompt(&self) -> String {
        self.status_prompt
            .clone()
            .unwrap_or_else(default_status_prompt)
    }

    pub fn configured_system_prompt(value: String) -> Result<Option<String>, String> {
        normalize_configured_prompt("server.system_prompt", value, default_system_prompt())
    }

    pub fn configured_status_prompt(value: String) -> Result<Option<String>, String> {
        normalize_configured_prompt("server.status_prompt", value, default_status_prompt())
    }

    fn normalize(&mut self) -> Result<(), String> {
        if let Some(system_prompt) = self.system_prompt.take() {
            self.system_prompt = Self::configured_system_prompt(system_prompt)?;
        }
        if let Some(status_prompt) = self.status_prompt.take() {
            self.status_prompt = Self::configured_status_prompt(status_prompt)?;
        }

        Ok(())
    }
}

fn normalize_configured_prompt(
    field: &str,
    value: String,
    default_value: String,
) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    if trimmed == default_value.trim() {
        return Ok(None);
    }

    Ok(Some(trimmed.to_string()))
}

impl LlmConfig {
    /// Resolve the API key
    pub fn resolve_api_key(&self) -> Option<String> {
        let env_var = match self.provider {
            LlmProvider::Gemini => "GEMINI_API_KEY",
            LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
            LlmProvider::OpenAi | LlmProvider::OpenAiCompatible => "OPENAI_API_KEY",
            LlmProvider::Echo => return None,
        };

        if let Ok(key) = std::env::var(env_var).or_else(|_| self.api_key.clone().ok_or(())) {
            if !key.is_empty() {
                return Some(key);
            }
        }

        None
    }
}

impl WeatherConfig {
    /// Resolve the PirateWeather API key
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Ok(key) = std::env::var("PIRATE_WEATHER_API_KEY") {
            if !key.is_empty() {
                return Some(key);
            }
        }
        self.pirate_weather_api_key
            .clone()
            .filter(|k| !k.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &tempfile::TempDir, file_name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(file_name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn missing_config_files_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();

        assert_eq!(config.llm.provider, LlmProvider::Echo);
        assert_eq!(config.llm.tools, LlmToolsConfig::default());
        assert_eq!(config.llm.memory, LlmMemoryConfig::default());
        assert!(config.llm.memory.enabled);
        assert_eq!(config.llm.memory.path, default_memory_path());
        assert_eq!(config.server.http_bind_addr, default_http_bind_addr());
        assert_eq!(config.server.system_prompt, None);
        assert_eq!(
            config.server.resolved_system_prompt(),
            default_system_prompt()
        );
        assert_eq!(config.server.status_prompt, None);
        assert_eq!(
            config.server.resolved_status_prompt(),
            default_status_prompt()
        );
        assert_eq!(config.storage.media_dir, default_media_dir());
        assert_eq!(config.llm.max_output_tokens, default_max_output_tokens());
    }

    #[test]
    fn parses_explicit_llm_max_output_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[llm]
max_output_tokens = 2048
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.llm.max_output_tokens, 2048);
    }

    #[test]
    fn omitted_system_prompt_tracks_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
public_addr = "192.0.2.10:8080"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.server.system_prompt, None);
        assert_eq!(
            config.server.resolved_system_prompt(),
            default_system_prompt()
        );
    }

    #[test]
    fn custom_system_prompt_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
system_prompt = "Custom prompt"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.server.system_prompt, Some("Custom prompt".into()));
        assert_eq!(config.server.resolved_system_prompt(), "Custom prompt");
    }

    #[test]
    fn empty_system_prompt_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
system_prompt = "   "
"#,
        );

        let error = Config::load(&path).unwrap_err().to_string();

        assert!(error.contains("server.system_prompt cannot be empty"));
    }

    #[test]
    fn custom_prompts_are_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
system_prompt = "  Custom system prompt  "
status_prompt = "  Custom status prompt  "
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(
            config.server.system_prompt,
            Some("Custom system prompt".into())
        );
        assert_eq!(
            config.server.status_prompt,
            Some("Custom status prompt".into())
        );
    }

    #[test]
    fn prompts_matching_defaults_are_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            &format!(
                r#"
[server]
system_prompt = "  {}  "
status_prompt = """
{}
"""
"#,
                default_system_prompt(),
                default_status_prompt()
            ),
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.server.system_prompt, None);
        assert_eq!(config.server.status_prompt, None);
    }

    #[test]
    fn omitted_status_prompt_tracks_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
public_addr = "192.0.2.10:8080"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.server.status_prompt, None);
        assert_eq!(
            config.server.resolved_status_prompt(),
            default_status_prompt()
        );
    }

    #[test]
    fn empty_status_prompt_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
status_prompt = "   "
"#,
        );

        let error = Config::load(&path).unwrap_err().to_string();

        assert!(error.contains("server.status_prompt cannot be empty"));
    }

    #[test]
    fn loads_partial_llm_tools_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[llm.tools]
enabled = false
max_tool_turns = 3
"#,
        );

        let config = Config::load(&path).unwrap();

        assert!(!config.llm.tools.enabled);
        assert_eq!(config.llm.tools.max_tool_turns, 3);
        assert_eq!(
            config.llm.tools.dynamic_tool_count,
            default_dynamic_tool_count()
        );
        assert_eq!(
            config.llm.tools.tool_concurrency,
            default_tool_concurrency()
        );
    }

    #[test]
    fn loads_partial_llm_memory_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[llm.memory]
path = "/tmp/assistant-memory.mv2"
top_k = 3
"#,
        );

        let config = Config::load(&path).unwrap();

        assert!(config.llm.memory.enabled);
        assert_eq!(config.llm.memory.path, "/tmp/assistant-memory.mv2");
        assert_eq!(config.llm.memory.top_k, 3);
        assert_eq!(
            config.llm.memory.snippet_chars,
            default_memory_snippet_chars()
        );
        assert_eq!(
            config.llm.memory.max_context_chars,
            default_memory_max_context_chars()
        );
        assert!(config.llm.memory.auto_retrieve);
        assert!(!config.llm.memory.auto_remember);
    }

    #[test]
    fn loads_base_config_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[llm]
provider = "openai"
model = "gpt-4.1-mini"

[server]
public_addr = "192.0.2.10:8080"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.llm.provider, LlmProvider::OpenAi);
        assert_eq!(config.llm.model, "gpt-4.1-mini");
        assert_eq!(config.server.public_addr, "192.0.2.10:8080");
        assert_eq!(config.server.http_bind_addr, default_http_bind_addr());
    }

    #[test]
    fn local_config_overrides_base_scalars() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[llm]
provider = "echo"
model = "base-model"
"#,
        );
        write_config(
            &dir,
            "config.local.toml",
            r#"
[llm]
provider = "anthropic"
model = "local-model"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.llm.provider, LlmProvider::Anthropic);
        assert_eq!(config.llm.model, "local-model");
    }

    #[test]
    fn local_config_merges_nested_tables_without_replacing_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "custom.toml",
            r#"
[server]
http_bind_addr = "0.0.0.0:8081"
grpc_bind_addr = "127.0.0.1:9091"
public_addr = "base.example:8081"
"#,
        );
        write_config(
            &dir,
            "config.local.toml",
            r#"
[server]
public_addr = "local.example:8081"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.server.http_bind_addr, "0.0.0.0:8081");
        assert_eq!(config.server.grpc_bind_addr, "127.0.0.1:9091");
        assert_eq!(config.server.public_addr, "local.example:8081");
    }

    #[test]
    fn local_only_partial_config_preserves_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.toml");
        write_config(
            &dir,
            "config.local.toml",
            r#"
[storage]
media_dir = "./local-media"
"#,
        );

        let config = Config::load(&path).unwrap();

        assert_eq!(config.storage.media_dir, "./local-media");
        assert_eq!(config.storage.db_path, default_db_path());
        assert_eq!(config.server.http_bind_addr, default_http_bind_addr());
    }

    #[test]
    fn invalid_local_config_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "custom.toml", "[llm]\nprovider = \"echo\"\n");
        write_config(&dir, "config.local.toml", "[llm\nprovider = \"openai\"\n");

        assert!(Config::load(&path).is_err());
    }
}
