use std::sync::Arc;

use crate::llm::backend::{LlmBackend, LlmFuture};
use crate::llm::request::LlmChatRequest;
use crate::llm::ChatResult;

pub struct EchoProvider;

impl EchoProvider {
    pub fn build() -> Arc<dyn LlmBackend> {
        tracing::info!("using echo backend (no LLM API calls)");
        Arc::new(EchoBackend)
    }
}

struct EchoBackend;

impl LlmBackend for EchoBackend {
    fn chat<'a>(&'a self, request: LlmChatRequest) -> LlmFuture<'a> {
        Box::pin(async move {
            match &request.image {
                Some(image_bytes) => Ok(ChatResult::Text(format!(
                    "Echo: [image] {} ({} bytes)",
                    request.utterance,
                    image_bytes.len()
                ))),
                None => Ok(ChatResult::Text(format!("Echo: {}", request.utterance))),
            }
        })
    }
}
