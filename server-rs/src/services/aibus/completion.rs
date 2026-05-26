use std::sync::Arc;

use prost::Message as _;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::config::Config;
use crate::llm::{LlmAgent, LlmChatRequest, PromptTemplateContext, PromptTemplates};
use crate::proto::aibus::*;
use crate::proto::common::encryption::EncryptedData;

pub struct CompletionHandler {
    agent: Arc<RwLock<Arc<LlmAgent>>>,
    config: Arc<RwLock<Config>>,
}

impl CompletionHandler {
    pub fn new(agent: Arc<RwLock<Arc<LlmAgent>>>, config: Arc<RwLock<Config>>) -> Self {
        Self { agent, config }
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
        let config = self.config.read().await.clone();
        let agent = self.agent.read().await.clone();
        let response_text = agent
            .chat(LlmChatRequest::new(
                prompt,
                Vec::new(),
                PromptTemplates {
                    system_prompt: config.server.system_prompt.clone(),
                    status_prompt: config.server.status_prompt.clone(),
                },
                PromptTemplateContext::new(
                    "encrypted-chat-completion",
                    &config,
                    chrono::Local::now(),
                ),
            ))
            .await
            .unwrap_or_else(|error| {
                warn!(error = %error, "EncryptedChatCompletion LLM failed");
                error
            });

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
        let config = self.config.read().await.clone();
        let agent = self.agent.read().await.clone();
        let response_text = agent
            .chat(LlmChatRequest::new(
                completion_req.prompt,
                Vec::new(),
                PromptTemplates {
                    system_prompt: config.server.system_prompt.clone(),
                    status_prompt: config.server.status_prompt.clone(),
                },
                PromptTemplateContext::new("encrypted-completion", &config, chrono::Local::now()),
            ))
            .await
            .unwrap_or_else(|error| {
                warn!(error = %error, "EncryptedCompletion LLM failed");
                error
            });

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
