use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::agent::AgentBuilder;
use rig::client::CompletionClient;
use rig::providers;
use rig::providers::openai::responses_api::ResponsesToolDefinition;
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

        // Only real OpenAI (not "openai-compatible" custom endpoints) supports the
        // Responses API and its hosted web_search tool.
        let web_search_enabled =
            llm_config.provider == LlmProvider::OpenAi && llm_config.web_search;

        if web_search_enabled {
            let mut builder = providers::openai::Client::builder()
                .api_key(&api_key)
                .http_client(http_client.clone());
            if let Some(ref base_url) = llm_config.base_url {
                builder = builder.base_url(base_url);
            }
            let client = builder.build()?;

            info!(
                "OpenAI agent ready (model={}, web_search=true)",
                llm_config.model
            );

            let model = client
                .completion_model(&llm_config.model)
                .with_tool(ResponsesToolDefinition::web_search());
            let agent_builder = AgentBuilder::new(model);

            RigBackend::from_agent_builder(
                "OpenAI",
                agent_builder,
                request_logger,
                config,
                http_client,
                memory,
            )
            .await
        } else {
            let mut builder = providers::openai::CompletionsClient::builder()
                .api_key(&api_key)
                .http_client(http_client.clone());
            if let Some(ref base_url) = llm_config.base_url {
                builder = builder.base_url(base_url);
            }
            let client = builder.build()?;

            info!(
                "OpenAI agent ready (model={}, custom_base={})",
                llm_config.model,
                llm_config.base_url.is_some()
            );
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
