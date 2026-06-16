use std::sync::Arc;

use reqwest::Client as HttpClient;
use tracing::info;

use crate::config::ResolvedConfig;

use super::ChatResult;
use super::backend::LlmBackend;
use super::memory::MemoryService;
use super::providers;
use super::request::LlmChatRequest;
use super::request_log::LlmRequestLogger;

/// Hot-swappable LLM agent facade backed by an object-safe provider adapter.
pub struct LlmAgent {
    backend: Arc<dyn LlmBackend>,
}

impl LlmAgent {
    /// Build an `LlmAgent` from the loaded config.
    pub async fn from_config(
        config: &ResolvedConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
        memory: Option<MemoryService>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let provider = config.config.llm.provider;
        info!(provider = %provider, model = %config.config.llm.model, "constructing LLM agent");

        let backend = providers::build_backend(config, http_client, request_logger, memory).await?;

        Ok(Self { backend })
    }

    /// Send a prompt with request-scoped conversation and system prompt context to the LLM.
    pub async fn chat(&self, request: LlmChatRequest) -> Result<ChatResult, String> {
        self.backend.chat(request).await
    }
}
