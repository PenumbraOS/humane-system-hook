use std::sync::Arc;

use rig::agent::{AgentBuilder, PromptHook};
use rig::completion::CompletionModel;
use rig::embeddings::EmbeddingsBuilder;
use rig::tool::ToolSet;
use rig::vector_store::in_memory_store::{InMemoryVectorIndex, InMemoryVectorStore};
use tracing::{info, warn};

use crate::config::{LlmConfig, ResolvedConfig};
use crate::external::osm::OsmClient;
use crate::external::weather::WeatherClient;
use crate::nearby::NearbyClient;

use super::fastembed;
use super::nearby_search::NearbySearchTool;
use super::reverse_geocode::ReverseGeocodeTool;
use super::weather::WeatherTool;

#[derive(Clone)]
pub struct LlmToolContext {
    pub nearby_client: Arc<NearbyClient>,
    pub osm: OsmClient,
    pub weather: WeatherClient,
}

impl LlmToolContext {
    pub fn new(http_client: reqwest::Client, config: &ResolvedConfig) -> Self {
        Self {
            nearby_client: Arc::new(NearbyClient::new(http_client.clone())),
            osm: OsmClient::new(http_client.clone()),
            weather: WeatherClient::new(http_client, config.pirate_weather_api_key.clone()),
        }
    }

    pub async fn build_tool_resources(
        &self,
        config: &LlmConfig,
    ) -> Result<Option<ToolResources>, String> {
        let tools_config = &config.tools;
        if !tools_config.enabled {
            info!("LLM native tools disabled by config");
            return Ok(None);
        }

        if tools_config.dynamic_tool_count == 0 {
            warn!(
                "LLM native tools enabled but dynamic_tool_count is 0; no tools will be attached"
            );

            return Ok(None);
        }

        let builder = ToolSet::builder()
            .dynamic_tool(NearbySearchTool::new(self.nearby_client.clone()))
            .dynamic_tool(ReverseGeocodeTool::new(self.osm.clone()));

        let builder = if self.weather.is_configured() {
            builder.dynamic_tool(WeatherTool::new(self.weather.clone()))
        } else {
            builder
        };

        let toolset = builder.build();

        let schemas = toolset.schemas().map_err(|err| err.to_string())?;
        if schemas.is_empty() {
            warn!("LLM native tools enabled but registry produced no dynamic tool schemas");
            return Ok(None);
        }

        let embedding_model = fastembed::build_embedding_model()?;
        let embeddings = EmbeddingsBuilder::new(embedding_model.clone())
            .documents(schemas)
            .map_err(|err| err.to_string())?
            .build()
            .await
            .map_err(|err| err.to_string())?;

        let vector_store: InMemoryVectorStore<rig::embeddings::ToolSchema> =
            InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
        let index = vector_store.index(embedding_model);

        info!(
            dynamic_tool_count = tools_config.dynamic_tool_count,
            "LLM native dynamic tools ready"
        );

        Ok(Some(ToolResources {
            sample_count: tools_config.dynamic_tool_count,
            index,
            toolset,
        }))
    }
}

pub struct ToolResources {
    pub sample_count: usize,
    pub index: InMemoryVectorIndex<rig_fastembed::EmbeddingModel, rig::embeddings::ToolSchema>,
    pub toolset: ToolSet,
}

impl ToolResources {
    pub fn apply<M, P>(
        self,
        builder: AgentBuilder<M, P>,
    ) -> AgentBuilder<M, P, rig::agent::WithBuilderTools>
    where
        M: CompletionModel,
        P: PromptHook<M>,
    {
        builder.dynamic_tools(self.sample_count, self.index, self.toolset)
    }
}
