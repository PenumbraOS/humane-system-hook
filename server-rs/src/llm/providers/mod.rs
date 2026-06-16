mod anthropic;
mod echo;
mod gemini;
mod openai;

use reqwest::Client as HttpClient;
use std::sync::Arc;

use crate::config::{LlmProvider, ResolvedConfig};
use crate::llm::backend::LlmBackend;
use crate::llm::memory::MemoryService;
use crate::llm::providers::anthropic::AnthropicProvider;
use crate::llm::providers::echo::EchoProvider;
use crate::llm::providers::gemini::GeminiProvider;
use crate::llm::providers::openai::OpenAiProvider;
use crate::llm::request_log::LlmRequestLogger;

pub async fn build_backend(
    config: &ResolvedConfig,
    http_client: HttpClient,
    request_logger: LlmRequestLogger,
    memory: Option<MemoryService>,
) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error + Send + Sync>> {
    match config.config.llm.provider {
        LlmProvider::Echo => Ok(EchoProvider::build()),
        LlmProvider::Gemini => {
            GeminiProvider::build(config, http_client, request_logger, memory).await
        }
        LlmProvider::Anthropic => {
            AnthropicProvider::build(config, http_client, request_logger, memory).await
        }
        LlmProvider::OpenAi | LlmProvider::OpenAiCompatible => {
            OpenAiProvider::build(config, http_client, request_logger, memory).await
        }
    }
}
