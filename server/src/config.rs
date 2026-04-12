use serde::Deserialize;
use std::path::Path;
use tracing::info;

/// Top-level configuration, loaded from `config.toml`.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// gRPC listen port.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Public address the device will use to reach this server (e.g. "192.168.1.125:9090").
    /// Used for constructing upload URLs. Falls back to the bind address if not set.
    pub public_addr: Option<String>,

    /// System prompt sent to the LLM.
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

    /// Display name shown during onboarding welcome screen.
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    /// Directory for storing captured media files.
    #[serde(default = "default_media_dir")]
    pub media_dir: String,
}

// --- defaults ---

fn default_provider() -> String {
    "echo".into()
}

fn default_model() -> String {
    "gemini-2.5-flash".into()
}

fn default_port() -> u16 {
    9090
}

fn default_system_prompt() -> String {
    "You are a helpful assistant running on a Humane AI Pin. Keep responses concise - they will be displayed on a laser projector and spoken aloud.".into()
}

fn default_media_dir() -> String {
    "./media".into()
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
            port: default_port(),
            public_addr: None,
            system_prompt: default_system_prompt(),
            display_name: None,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            media_dir: default_media_dir(),
        }
    }
}

impl Config {
    /// Load config from file. Falls back to defaults if file is missing.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let config: Config = toml::from_str(&contents)?;
            info!(?path, "loaded config");
            Ok(config)
        } else {
            info!(?path, "config file not found, using defaults");
            Ok(Config {
                llm: LlmConfig::default(),
                server: ServerConfig::default(),
                storage: StorageConfig::default(),
            })
        }
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
