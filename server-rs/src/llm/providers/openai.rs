use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::prelude::*;
use rig::providers;
use tracing::info;

use crate::config::LlmConfig;
use crate::llm::backend::LlmBackend;
use crate::llm::request_log::LlmRequestLogger;
use crate::llm::rig_backend::RigBackend;

pub struct OpenAiProvider;

impl OpenAiProvider {
    pub fn build(
        config: &LlmConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error>> {
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
        let agent = client.agent(&config.model).build();
        info!(
            "OpenAI agent ready (model={}, custom_base={})",
            config.model,
            config.base_url.is_some()
        );
        Ok(RigBackend::arc("OpenAI", agent, request_logger))
    }
}
