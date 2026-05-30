use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::prelude::*;
use rig::providers;
use tracing::info;

use crate::config::LlmConfig;
use crate::llm::backend::LlmBackend;
use crate::llm::request_log::LlmRequestLogger;
use crate::llm::rig_backend::RigBackend;

pub struct AnthropicProvider;

impl AnthropicProvider {
    pub fn build(
        config: &LlmConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error>> {
        let api_key = config.resolve_api_key().ok_or(
            "Anthropic api_key not set; configure ANTHROPIC_API_KEY in the environment or .env, or set llm.api_key in config.toml",
        )?;
        let client = providers::anthropic::Client::builder()
            .api_key(&api_key)
            .http_client(http_client)
            .build()?;
        let agent = client.agent(&config.model).build();
        info!("Anthropic agent ready (model={})", config.model);
        Ok(RigBackend::arc("Anthropic", agent, request_logger))
    }
}
