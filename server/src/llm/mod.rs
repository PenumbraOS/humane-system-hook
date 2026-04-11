use rig::completion::message::{Message, UserContent, ImageMediaType};
use rig::prelude::*; // imports CompletionClient trait for .agent()
use rig::providers;
use rig::OneOrMany;
use tracing::{error, info};

use crate::config::LlmConfig;

/// Enum-dispatched LLM agent. Each variant wraps a concrete rig agent type
/// (the `Prompt` trait is not object-safe due to RPITIT).
pub enum LlmAgent {
    Echo,
    Gemini(rig::agent::Agent<providers::gemini::CompletionModel>),
    Anthropic(rig::agent::Agent<providers::anthropic::completion::CompletionModel>),
    /// OpenAI Chat Completions API — also used for openai-compatible providers.
    OpenAi(rig::agent::Agent<providers::openai::CompletionModel>),
}

impl LlmAgent {
    /// Build an `LlmAgent` from the loaded config.
    pub fn from_config(
        config: &LlmConfig,
        system_prompt: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let provider = config.provider.to_lowercase();
        info!(provider = %provider, model = %config.model, "constructing LLM agent");

        match provider.as_str() {
            "echo" => {
                info!("using echo backend (no LLM API calls)");
                Ok(LlmAgent::Echo)
            }
            "gemini" => {
                let api_key = config.resolve_api_key()
                    .ok_or("Gemini api_key not set in config.toml")?;
                let client = providers::gemini::Client::new(&api_key)?;
                let agent = client
                    .agent(&config.model)
                    .preamble(system_prompt)
                    .build();
                info!("Gemini agent ready (model={})", config.model);
                Ok(LlmAgent::Gemini(agent))
            }
            "anthropic" => {
                let api_key = config.resolve_api_key()
                    .ok_or("Anthropic api_key not set in config.toml")?;
                let client = providers::anthropic::Client::new(&api_key)?;
                let agent = client
                    .agent(&config.model)
                    .preamble(system_prompt)
                    .build();
                info!("Anthropic agent ready (model={})", config.model);
                Ok(LlmAgent::Anthropic(agent))
            }
            "openai" | "openai-compatible" => {
                let api_key = config.resolve_api_key()
                    .ok_or("OpenAI api_key not set in config.toml")?;
                let client = if let Some(ref base_url) = config.base_url {
                    providers::openai::CompletionsClient::builder()
                        .api_key(&api_key)
                        .base_url(base_url)
                        .build()?
                } else {
                    providers::openai::CompletionsClient::new(&api_key)?
                };
                let agent = client
                    .agent(&config.model)
                    .preamble(system_prompt)
                    .build();
                info!("OpenAI agent ready (model={}, custom_base={})", config.model, config.base_url.is_some());
                Ok(LlmAgent::OpenAi(agent))
            }
            other => {
                Err(format!("unknown LLM provider: '{}' (valid: echo, gemini, anthropic, openai, openai-compatible)", other).into())
            }
        }
    }

    /// Send a prompt with conversation history to the LLM.
    /// Falls back to simple prompt for Echo backend or empty history.
    pub async fn chat(&self, utterance: &str, history: Vec<Message>) -> Result<String, String> {
        use rig::completion::Chat;

        if history.is_empty() {
            return self.prompt(utterance).await;
        }

        match self {
            LlmAgent::Echo => Ok(format!("Echo: {}", utterance)),
            LlmAgent::Gemini(agent) => agent.chat(utterance, history).await.map_err(|e| {
                error!(error = %e, "Gemini chat failed");
                format!("Gemini error: {}", e)
            }),
            LlmAgent::Anthropic(agent) => agent.chat(utterance, history).await.map_err(|e| {
                error!(error = %e, "Anthropic chat failed");
                format!("Anthropic error: {}", e)
            }),
            LlmAgent::OpenAi(agent) => agent.chat(utterance, history).await.map_err(|e| {
                error!(error = %e, "OpenAI chat failed");
                format!("OpenAI error: {}", e)
            }),
        }
    }

    /// Send a single prompt with no conversation history.
    pub async fn prompt(&self, utterance: &str) -> Result<String, String> {
        use rig::completion::Prompt;

        match self {
            LlmAgent::Echo => Ok(format!("Echo: {}", utterance)),
            LlmAgent::Gemini(agent) => agent.prompt(utterance).await.map_err(|e| {
                error!(error = %e, "Gemini prompt failed");
                format!("Gemini error: {}", e)
            }),
            LlmAgent::Anthropic(agent) => agent.prompt(utterance).await.map_err(|e| {
                error!(error = %e, "Anthropic prompt failed");
                format!("Anthropic error: {}", e)
            }),
            LlmAgent::OpenAi(agent) => agent.prompt(utterance).await.map_err(|e| {
                error!(error = %e, "OpenAI prompt failed");
                format!("OpenAI error: {}", e)
            }),
        }
    }

    /// Send a vision prompt with an image (base64-encoded JPEG) and a question.
    pub async fn vision_prompt(&self, question: &str, image_base64: &str) -> Result<String, String> {
        use rig::completion::Prompt;

        let message = Message::User {
            content: OneOrMany::many(vec![
                UserContent::text(question),
                UserContent::image_base64(
                    image_base64,
                    Some(ImageMediaType::JPEG),
                    None, // detail: auto
                ),
            ]).expect("non-empty content vec"),
        };

        match self {
            LlmAgent::Echo => Ok(format!("Echo: [vision] {}", question)),
            LlmAgent::Gemini(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "Gemini vision prompt failed");
                format!("Gemini error: {}", e)
            }),
            LlmAgent::Anthropic(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "Anthropic vision prompt failed");
                format!("Anthropic error: {}", e)
            }),
            LlmAgent::OpenAi(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "OpenAI vision prompt failed");
                format!("OpenAI error: {}", e)
            }),
        }
    }
}
