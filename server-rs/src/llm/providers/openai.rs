use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::providers;
use tracing::info;

use crate::config::{LlmProvider, ResolvedConfig};
use crate::llm::backend::LlmBackend;
use crate::llm::memory::MemoryService;
use crate::llm::request_log::LlmRequestLogger;
use crate::llm::rig_backend::RigBackend;

pub struct OpenAiProvider;

impl OpenAiProvider {
    pub async fn build(
        config: &ResolvedConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
        memory: Option<MemoryService>,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let llm_config = &config.config.llm;
        let api_key = llm_config.resolve_api_key().ok_or(
            "OpenAI api_key not set; configure OPENAI_API_KEY in the environment or .env, or set llm.api_key in config.toml",
        )?;

        // OpenAI uses the Responses API, but "openai-compatible" custom endpoints stay on the Completions API.
        let use_responses_api = llm_config.provider == LlmProvider::OpenAi;
        // The hosted web_search tool is only available on the Responses API.
        let web_search_enabled = use_responses_api && llm_config.web_search;

        let mut builder = providers::openai::CompletionsClient::builder()
            .api_key(&api_key)
            .http_client(http_client.clone());
        if let Some(ref base_url) = llm_config.base_url {
            builder = builder.base_url(base_url);
        }
        let client = builder.build()?;

        info!(
            "OpenAI agent ready (model={}, api={}, custom_base={}, web_search={})",
            llm_config.model,
            if use_responses_api {
                "responses"
            } else {
                "completions"
            },
            llm_config.base_url.is_some(),
            web_search_enabled
        );

        if use_responses_api {
            RigBackend::from_client(
                "OpenAI",
                client.responses_api(),
                request_logger,
                config,
                http_client,
                memory,
                |builder| {
                    if web_search_enabled {
                        builder.additional_params(serde_json::json!({
                            "tools": [{ "type": "web_search" }]
                        }))
                    } else {
                        builder
                    }
                },
            )
            .await
        } else {
            RigBackend::from_client(
                "OpenAI",
                client,
                request_logger,
                config,
                http_client,
                memory,
                |builder| builder,
            )
            .await
        }
    }
}
