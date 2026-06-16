use std::sync::Arc;

use prost::Message as _;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::config::ResolvedConfig;
use crate::llm::ChatResult;
use crate::llm::memory::MemoryService;
use crate::llm::{LlmAgent, LlmChatRequest, PromptTemplateContext, PromptTemplates};
use crate::proto::aibus::*;
use crate::proto::common::encryption::EncryptedData;

pub struct CompletionHandler {
    agent: Arc<LlmAgent>,
    config: Arc<ResolvedConfig>,
    memory: Option<MemoryService>,
}

impl CompletionHandler {
    pub fn new(
        agent: Arc<LlmAgent>,
        config: Arc<ResolvedConfig>,
        memory: Option<MemoryService>,
    ) -> Self {
        Self {
            agent,
            config,
            memory,
        }
    }

    async fn retrieve_memory_context(&self, prompt: &str) -> Option<String> {
        let Some(memory) = &self.memory else {
            return None;
        };

        match memory.retrieve_context(prompt.to_string()).await {
            Ok(context) => context,
            Err(error) => {
                warn!(error = %error, "memory retrieval failed");
                None
            }
        }
    }

    pub async fn encrypted_chat_completion(
        &self,
        request: Request<EncryptedChatCompletionRequest>,
    ) -> Result<Response<EncryptedChatCompletionResponse>, Status> {
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let chat_req = ChatCompletionRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad ChatCompletionRequest: {e}")))?;
        let prompt = chat_req
            .messages
            .iter()
            .filter(|m| !m.content.trim().is_empty())
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = if prompt.is_empty() {
            "Hello".to_string()
        } else {
            prompt
        };

        info!(
            messages = chat_req.messages.len(),
            ">>> EncryptedChatCompletion"
        );
        let memory_context = self.retrieve_memory_context(&prompt).await;
        let response_text = match self
            .agent
            .chat(LlmChatRequest::new(
                prompt,
                Vec::new(),
                PromptTemplates {
                    system_prompt: self.config.config.server.system_prompt.clone(),
                    status_prompt: self.config.config.server.status_prompt.clone(),
                },
                PromptTemplateContext::new(
                    "encrypted-chat-completion",
                    &self.config,
                    chrono::Local::now(),
                ),
                memory_context,
            ))
            .await
        {
            Ok(ChatResult::Text(text)) => text,
            Ok(ChatResult::DeferredVision) => {
                warn!("EncryptedChatCompletion triggered vision tool (unexpected)");
                "I can't capture an image in this context.".to_string()
            }
            Err(error) => {
                warn!(error = %error, "EncryptedChatCompletion LLM failed");
                error
            }
        };

        let chat_response = ChatCompletionResponse {
            choices: vec![Choice {
                message: Some(ChatCompletionMessage {
                    role: "assistant".into(),
                    content: response_text,
                    tool_calls: vec![],
                    name: String::new(),
                    tool_call_id: String::new(),
                }),
                stop_reason: "stop".into(),
            }],
            usage: Some(ChatCompletionUsage::default()),
            error: None,
        };

        Ok(Response::new(EncryptedChatCompletionResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.ChatCompletionResponse",
                chat_response.encode_to_vec(),
            )),
        }))
    }

    pub async fn encrypted_completion(
        &self,
        request: Request<EncryptedCompletionRequest>,
    ) -> Result<Response<EncryptedCompletionResponse>, Status> {
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let completion_req = CompletionRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad CompletionRequest: {e}")))?;

        info!(
            prompt_len = completion_req.prompt.len(),
            ">>> EncryptedCompletion"
        );
        let prompt = completion_req.prompt;
        let memory_context = self.retrieve_memory_context(&prompt).await;
        let response_text = match self
            .agent
            .chat(LlmChatRequest::new(
                prompt,
                Vec::new(),
                PromptTemplates {
                    system_prompt: self.config.config.server.system_prompt.clone(),
                    status_prompt: self.config.config.server.status_prompt.clone(),
                },
                PromptTemplateContext::new(
                    "encrypted-completion",
                    &self.config,
                    chrono::Local::now(),
                ),
                memory_context,
            ))
            .await
        {
            Ok(ChatResult::Text(text)) => text,
            Ok(ChatResult::DeferredVision) => {
                warn!("EncryptedCompletion triggered vision tool (unexpected)");
                "I can't capture an image in this context.".to_string()
            }
            Err(error) => {
                warn!(error = %error, "EncryptedCompletion LLM failed");
                error
            }
        };

        let completion_response = CompletionResponse {
            choices: vec![CompletionChoice {
                text: response_text,
                index: 0,
                finish_reason: "stop".into(),
            }],
            usage: Some(CompletionUsage::default()),
            error: None,
        };

        Ok(Response::new(EncryptedCompletionResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.CompletionResponse",
                completion_response.encode_to_vec(),
            )),
        }))
    }
}
