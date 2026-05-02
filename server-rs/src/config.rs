use serde::{Deserialize, Serialize};
use std::path::Path;
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
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LlmConfig {
    /// Provider name: "gemini", "anthropic", "openai", "openai-compatible", "echo"
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Model ID for the chosen provider (e.g. "gemini-2.5-flash")
    #[serde(default = "default_model")]
    pub model: String,

    /// API key — overrides the corresponding env var if set.
    pub api_key: Option<String>,

    /// Base URL — only used for "openai-compatible" provider.
    pub base_url: Option<String>,
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

    /// System prompt sent to the LLM.
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

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

fn default_provider() -> String {
    "echo".into()
}

fn default_model() -> String {
    "gemini-2.5-flash".into()
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
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_bind_addr: default_http_bind_addr(),
            grpc_bind_addr: default_grpc_bind_addr(),
            public_addr: default_public_addr(),
            system_prompt: default_system_prompt(),
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
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let config = if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let config: Config = toml::from_str(&contents)?;
            info!(?path, "loaded config");
            config
        } else {
            info!(?path, "config file not found, using defaults");
            Config {
                llm: LlmConfig::default(),
                server: ServerConfig::default(),
                storage: StorageConfig::default(),
                weather: WeatherConfig::default(),
                logging: LoggingConfig::default(),
            }
        };

        #[cfg(target_os = "android")]
        {
            if !std::path::Path::new(&config.storage.media_dir).is_absolute() {
                return Err(format!("Android requires absolute storage.media_dir, got {}", config.storage.media_dir).into());
            }
            if !std::path::Path::new(&config.storage.db_path).is_absolute() {
                return Err(format!("Android requires absolute storage.db_path, got {}", config.storage.db_path).into());
            }
        }

        Ok(config)
    }
}

impl LlmConfig {
    /// Resolve the API key
    pub fn resolve_api_key(&self) -> Option<String> {
        let env_var = match self.provider.as_str() {
            "gemini" => "GEMINI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" | "openai-compatible" => "OPENAI_API_KEY",
            _ => return None,
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
