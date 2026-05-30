use std::sync::Arc;
use std::time::Instant;

use rig::agent::{Agent, PromptHook};
use rig::completion::CompletionModel;
use rig::completion::{Chat, Prompt};
use tracing::error;

use super::backend::{LlmBackend, LlmFuture};
use super::error::friendly_error_message;
use super::prompt::PromptBuilder;
use super::providers::vision_message;
use super::request::LlmChatRequest;
use super::request_log::LlmRequestLogger;

/// Shared LLM backend for providers
pub struct RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    provider_label: &'static str,
    agent: Agent<M>,
    request_logger: LlmRequestLogger,
}

impl<M> RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    pub fn new(
        provider_label: &'static str,
        agent: Agent<M>,
        request_logger: LlmRequestLogger,
    ) -> Self {
        Self {
            provider_label,
            agent,
            request_logger,
        }
    }

    pub fn arc(
        provider_label: &'static str,
        agent: Agent<M>,
        request_logger: LlmRequestLogger,
    ) -> Arc<dyn LlmBackend> {
        Arc::new(Self::new(provider_label, agent, request_logger))
    }
}

impl<M> LlmBackend for RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    fn chat<'a>(&'a self, request: LlmChatRequest) -> LlmFuture<'a> {
        Box::pin(async move {
            let utterance = request.utterance.clone();
            let run_id = request.template_context.run_id.clone();
            let history = PromptBuilder::build_chat_history(&request);
            let started = Instant::now();

            let result = self.agent.chat(utterance.clone(), history.clone()).await;
            let latency_ms = started.elapsed().as_millis();

            let result = result.map_err(|e| {
                error!(provider = self.provider_label, error = %e, "LLM chat failed");
                friendly_error_message(&e)
            });

            self.request_logger
                .log_chat(
                    self.provider_label,
                    &run_id,
                    &history,
                    &utterance,
                    result.clone().ok().as_deref(),
                    result.clone().err().as_deref(),
                    latency_ms,
                )
                .await;

            result
        })
    }

    fn vision_prompt<'a>(&'a self, question: &'a str, image_base64: &'a str) -> LlmFuture<'a> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self
                .agent
                .prompt(vision_message(question, image_base64))
                .await;
            let latency_ms = started.elapsed().as_millis();

            let result = result.map_err(|e| {
                error!(provider = self.provider_label, error = %e, "LLM vision prompt failed");
                friendly_error_message(&e)
            });

            self.request_logger
                .log_vision(
                    self.provider_label,
                    question,
                    image_base64.len(),
                    result.clone().ok().as_deref(),
                    result.clone().err().as_deref(),
                    latency_ms,
                )
                .await;

            result
        })
    }
}
