use rig::completion::message::Message;
use serde::Serialize;

use crate::config::ResolvedConfig;

#[derive(Clone, Debug)]
pub struct PromptTemplates {
    pub system_prompt: String,
    pub status_prompt: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptTemplateContext {
    pub run_id: String,
    pub assistant_display_name: Option<String>,
    pub server_public_addr: String,

    pub current_timestamp: String,
    pub current_date: String,
    pub current_time: String,

    pub location_name: Option<String>,
    pub latitude: Option<String>,
    pub longitude: Option<String>,
    pub coordinates: Option<String>,
}

impl PromptTemplateContext {
    pub fn new(
        run_id: &str,
        config: &ResolvedConfig,
        datetime: chrono::DateTime<chrono::Local>,
    ) -> Self {
        let current_timestamp = datetime.to_rfc3339();
        let current_date = datetime.format("%Y-%m-%d").to_string();
        let current_time = datetime.format("%H:%M:%S %z").to_string();

        Self {
            run_id: run_id.to_string(),
            assistant_display_name: config.config.server.display_name.clone(),
            server_public_addr: config.config.server.public_addr.clone(),

            current_timestamp,
            current_date,
            current_time,

            location_name: None,
            latitude: None,
            longitude: None,
            coordinates: None,
        }
    }
}

/// Request-scoped context for building an LLM call
pub struct LlmChatRequest {
    pub utterance: String,
    pub history: Vec<Message>,
    pub templates: PromptTemplates,
    pub template_context: PromptTemplateContext,
    pub memory_context: Option<String>,

    pub image: Option<Vec<u8>>,
}

impl LlmChatRequest {
    pub fn new(
        utterance: String,
        history: Vec<Message>,
        templates: PromptTemplates,
        template_context: PromptTemplateContext,
        memory_context: Option<String>,
    ) -> Self {
        Self {
            utterance,
            history,
            templates,
            template_context,
            memory_context,
            image: None,
        }
    }

    /// Attach an image asset to this request's new user turn
    pub fn with_image(mut self, image_bytes: Vec<u8>) -> Self {
        self.image = Some(image_bytes);
        self
    }
}
