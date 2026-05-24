use handlebars::{no_escape, Handlebars};
use rig::completion::message::Message;
use tracing::{info, warn};

use super::request::{LlmChatRequest, PromptTemplateContext};

/// Builds the provider-neutral prompt payload for a chat request.
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build_chat_history(request: &LlmChatRequest) -> Vec<Message> {
        let mut history = Vec::with_capacity(request.history.len() + 2);

        if let Some(system_prompt) = render_template(
            "system_prompt",
            &request.templates.system_prompt,
            &request.template_context,
        ) {
            info!("Sending system prompt:\n{system_prompt}");
            history.push(Message::system(system_prompt));
        }

        history.extend(request.history.iter().cloned());

        if let Some(status_prompt) = render_template(
            "status_prompt",
            &request.templates.status_prompt,
            &request.template_context,
        ) {
            info!("Sending status prompt:\n{status_prompt}");
            history.push(Message::system(status_prompt));
        }

        history
    }
}

pub fn validate_prompt_template(name: &str, template: &str) -> Result<(), String> {
    let mut registry = handlebars_registry();
    registry
        .register_template_string(name, template)
        .map_err(|error| error.to_string())
}

fn render_template(
    name: &'static str,
    template: &str,
    context: &PromptTemplateContext,
) -> Option<String> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }

    let registry = handlebars_registry();
    match registry.render_template(template, context) {
        Ok(rendered) => {
            let rendered = rendered.trim().to_string();
            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        }
        Err(error) => {
            warn!(template = name, error = %error, "failed to render prompt template");
            Some(template.to_string())
        }
    }
}

fn handlebars_registry() -> Handlebars<'static> {
    let mut registry = Handlebars::new();
    registry.register_escape_fn(no_escape);
    registry
}
