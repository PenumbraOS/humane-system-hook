mod agent;
mod backend;
mod error;
pub mod memory;
mod prompt;
mod providers;
mod request;
mod request_log;
mod rig_backend;
pub mod tools;

pub use agent::LlmAgent;
pub use prompt::validate_prompt_template;
pub use request::{LlmChatRequest, PromptTemplateContext, PromptTemplates};
pub use request_log::LlmRequestLogger;

/// Result of an LLM user Understand request
#[derive(Debug, Clone)]
pub enum ChatResult {
    Text(String),
    /// The `understand_scene` tool was invoked; the server awaits a follow up request containing a new camera image
    DeferredVision,
}
