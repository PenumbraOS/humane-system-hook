use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::prelude::*;
use rig::providers;
use tracing::info;

use crate::config::LlmConfig;
use crate::llm::backend::LlmBackend;
use crate::llm::request_log::LlmRequestLogger;
use crate::llm::rig_backend::RigBackend;

pub struct GeminiProvider;

impl GeminiProvider {
    pub fn build(
        config: &LlmConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error>> {
        let api_key = config.resolve_api_key().ok_or(
            "Gemini api_key not set; configure GEMINI_API_KEY in the environment or .env, or set llm.api_key in config.toml",
        )?;
        let client = providers::gemini::Client::builder()
            .api_key(&api_key)
            .http_client(http_client)
            .build()?;
        let mut builder = client.agent(&config.model);

        if config.gemini_google_search {
            // The Gemini provider's request builder forwards `tools` from
            // additional_params into the GenerateContent request.
            builder = builder.additional_params(serde_json::json!({
                "tools": [{ "google_search": {} }]
            }));
            info!("Gemini Google Search grounding enabled");
        }

        let agent = builder.build();
        info!("Gemini agent ready (model={})", config.model);
        Ok(RigBackend::arc("Gemini", agent, request_logger))
    }
}
