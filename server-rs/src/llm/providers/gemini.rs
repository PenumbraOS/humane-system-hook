use std::sync::Arc;

use reqwest::Client as HttpClient;
use rig::providers;
use tracing::info;

use crate::config::ResolvedConfig;
use crate::llm::backend::LlmBackend;
use crate::llm::memory::MemoryService;
use crate::llm::request_log::LlmRequestLogger;
use crate::llm::rig_backend::RigBackend;

pub struct GeminiProvider;

impl GeminiProvider {
    pub async fn build(
        config: &ResolvedConfig,
        http_client: HttpClient,
        request_logger: LlmRequestLogger,
        memory: Option<MemoryService>,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let llm_config = &config.config.llm;
        let api_key = llm_config.resolve_api_key().ok_or(
            "Gemini api_key not set; configure GEMINI_API_KEY in the environment or .env, or set llm.api_key in config.toml",
        )?;
        let client = providers::gemini::Client::builder()
            .api_key(&api_key)
            .http_client(http_client.clone())
            .build()?;

        info!("Gemini agent ready (model={})", llm_config.model);
        let gemini_google_search = llm_config.web_search;
        let tools_enabled = llm_config.tools.enabled;

        RigBackend::from_client(
            "Gemini",
            client,
            request_logger,
            config,
            http_client,
            memory,
            move |builder| {
                let mut additional_params = if gemini_google_search {
                    info!("Gemini Google Search grounding enabled");
                    // The Gemini provider's request builder forwards `tools` from
                    // additional_params into the GenerateContent request.
                    serde_json::json!({
                        "tools": [{ "google_search": {} }],
                    })
                } else {
                    serde_json::json!({})
                };

                if tools_enabled {
                    let tool_config = serde_json::json!({
                        "tool_config": {
                            "include_server_side_tool_invocations": true
                        }
                    });

                    additional_params
                        .as_object_mut()
                        .expect("Invalid Gemini additional_params")
                        .extend(
                            tool_config
                                .as_object()
                                .expect("Invalid Gemini tool_config")
                                .to_owned(),
                        );
                }

                builder.additional_params(additional_params)
            },
        )
        .await
    }
}
