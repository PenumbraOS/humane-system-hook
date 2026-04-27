use std::fmt::Display;

use reqwest::Client as HttpClient;
use rig::completion::message::{ImageMediaType, Message, UserContent};
use rig::prelude::*; // imports CompletionClient trait for .agent()
use rig::providers;
use rig::OneOrMany;
use tracing::{error, info};

use crate::config::LlmConfig;

/// Convert a raw LLM provider error into a friendly, speakable sentence.
fn friendly_error_message(e: &impl Display) -> String {
    let raw = e.to_string().to_lowercase();

    if raw.contains("429")
        || raw.contains("rate limit")
        || raw.contains("resource_exhausted")
        || raw.contains("too many requests")
    {
        "I'm getting too many requests right now. Please try again in a moment.".into()
    } else if raw.contains("401")
        || raw.contains("403")
        || raw.contains("unauthorized")
        || raw.contains("forbidden")
        || raw.contains("invalid api key")
        || raw.contains("permission denied")
    {
        "There's a problem with the API key configuration. Please check the server settings.".into()
    } else if raw.contains("404")
        || raw.contains("model not found")
        || raw.contains("not_found")
        || raw.contains("does not exist")
    {
        "The configured AI model wasn't found. Please check the server settings.".into()
    } else if raw.contains("500")
        || raw.contains("502")
        || raw.contains("503")
        || raw.contains("internal server error")
        || raw.contains("service unavailable")
        || raw.contains("bad gateway")
    {
        "The AI service is temporarily unavailable. Please try again shortly.".into()
    } else if raw.contains("timeout")
        || raw.contains("timed out")
        || raw.contains("deadline exceeded")
    {
        "The request to the AI service timed out. Please try again.".into()
    } else if raw.contains("connection")
        || raw.contains("dns")
        || raw.contains("resolve")
        || raw.contains("unreachable")
    {
        "I couldn't reach the AI service. Please check the server's internet connection.".into()
    } else if raw.contains("content filter")
        || raw.contains("safety")
        || raw.contains("blocked")
        || raw.contains("harm_category")
    {
        "The AI service declined to answer that. Try rephrasing your question.".into()
    } else if raw.contains("context length")
        || raw.contains("too long")
        || raw.contains("max tokens")
        || raw.contains("token limit")
    {
        "That conversation got too long for the AI service to handle. Try starting a new one."
            .into()
    } else {
        "Something went wrong while contacting the AI service. Please try again.".into()
    }
}

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
        http_client: HttpClient,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let provider = config.provider.to_lowercase();
        info!(provider = %provider, model = %config.model, "constructing LLM agent");

        match provider.as_str() {
            "echo" => {
                info!("using echo backend (no LLM API calls)");
                Ok(LlmAgent::Echo)
            }
            "gemini" => {
                let api_key = config.resolve_api_key().ok_or(
                    "Gemini api_key not set; configure GEMINI_API_KEY in the environment or .env, or set llm.api_key in config.toml",
                )?;
                let client = providers::gemini::Client::builder()
                    .api_key(&api_key)
                    .http_client(http_client.clone())
                    .build()?;
                let agent = client
                    .agent(&config.model)
                    .preamble(system_prompt)
                    .build();
                info!("Gemini agent ready (model={})", config.model);
                Ok(LlmAgent::Gemini(agent))
            }
            "anthropic" => {
                let api_key = config.resolve_api_key().ok_or(
                    "Anthropic api_key not set; configure ANTHROPIC_API_KEY in the environment or .env, or set llm.api_key in config.toml",
                )?;
                let client = providers::anthropic::Client::builder()
                    .api_key(&api_key)
                    .http_client(http_client.clone())
                    .build()?;
                let agent = client
                    .agent(&config.model)
                    .preamble(system_prompt)
                    .build();
                info!("Anthropic agent ready (model={})", config.model);
                Ok(LlmAgent::Anthropic(agent))
            }
            "openai" | "openai-compatible" => {
                let api_key = config.resolve_api_key().ok_or(
                    "OpenAI api_key not set; configure OPENAI_API_KEY in the environment or .env, or set llm.api_key in config.toml",
                )?;
                let mut builder = providers::openai::CompletionsClient::builder()
                    .api_key(&api_key)
                    .http_client(http_client);
                if let Some(ref base_url) = config.base_url {
                    builder = builder.base_url(base_url);
                }
                let client = builder.build()?;
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
                friendly_error_message(&e)
            }),
            LlmAgent::Anthropic(agent) => agent.chat(utterance, history).await.map_err(|e| {
                error!(error = %e, "Anthropic chat failed");
                friendly_error_message(&e)
            }),
            LlmAgent::OpenAi(agent) => agent.chat(utterance, history).await.map_err(|e| {
                error!(error = %e, "OpenAI chat failed");
                friendly_error_message(&e)
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
                friendly_error_message(&e)
            }),
            LlmAgent::Anthropic(agent) => agent.prompt(utterance).await.map_err(|e| {
                error!(error = %e, "Anthropic prompt failed");
                friendly_error_message(&e)
            }),
            LlmAgent::OpenAi(agent) => agent.prompt(utterance).await.map_err(|e| {
                error!(error = %e, "OpenAI prompt failed");
                friendly_error_message(&e)
            }),
        }
    }

    /// Send a vision prompt with an image (base64-encoded JPEG) and a question.
    pub async fn vision_prompt(
        &self,
        question: &str,
        image_base64: &str,
    ) -> Result<String, String> {
        use rig::completion::Prompt;

        let message = Message::User {
            content: OneOrMany::many(vec![
                UserContent::text(question),
                UserContent::image_base64(
                    image_base64,
                    Some(ImageMediaType::JPEG),
                    None, // detail: auto
                ),
            ])
            .expect("non-empty content vec"),
        };

        match self {
            LlmAgent::Echo => Ok(format!("Echo: [vision] {}", question)),
            LlmAgent::Gemini(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "Gemini vision prompt failed");
                friendly_error_message(&e)
            }),
            LlmAgent::Anthropic(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "Anthropic vision prompt failed");
                friendly_error_message(&e)
            }),
            LlmAgent::OpenAi(agent) => agent.prompt(message).await.map_err(|e| {
                error!(error = %e, "OpenAI vision prompt failed");
                friendly_error_message(&e)
            }),
        }
    }
}
