use std::pin::Pin;

use futures::StreamExt;
use prost::Message as _;
use rig::completion::message::Message;
use tokio_stream::Stream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

use std::sync::Arc;

use super::envelope::unwrap_plaintext_data;
use crate::config::ResolvedConfig;
use crate::db::Database;
use crate::llm::memory::MemoryService;
use crate::llm::ChatResult;
use crate::llm::{LlmAgent, LlmChatRequest, PromptTemplateContext, PromptTemplates};
use crate::proto::aibus::*;
use crate::proto::common::encryption::{self, EncryptedData};
use crate::synapse::conversation::extract_history;
use crate::synapse::extract_run_id;
use crate::synapse::image_store::LiveImageStore;
use crate::synapse::vision::{extract_most_recent_image_data, is_vision_request};

pub struct UnderstandHandler {
    agent: Arc<LlmAgent>,
    config: Arc<ResolvedConfig>,
    db: Database,
    memory: Option<MemoryService>,
    image_store: LiveImageStore,
}

impl UnderstandHandler {
    pub fn new(
        agent: Arc<LlmAgent>,
        config: Arc<ResolvedConfig>,
        db: Database,
        memory: Option<MemoryService>,
        image_store: LiveImageStore,
    ) -> Self {
        Self {
            agent,
            config,
            db,
            memory,
            image_store,
        }
    }

    fn build_prompt_template_context(
        &self,
        req: &SynapseUnderstandingRequest,
        run_id: &str,
        config: &ResolvedConfig,
    ) -> PromptTemplateContext {
        // TODO: Expose specific device fields, like battery level, wifi/cellular status, etc.
        let mut context = PromptTemplateContext::new(run_id, config, chrono::Local::now());

        if let Some(ctx) = req.device_context.as_ref() {
            context.location_name = non_empty_string(&ctx.reverse_geocoded_location);
        }

        if let Some(loc) = req.location.as_ref() {
            let latitude = format_coordinate(loc.latitude);
            let longitude = format_coordinate(loc.longitude);

            context.latitude = Some(latitude.clone());
            context.longitude = Some(longitude.clone());
            context.coordinates = Some(format!("{latitude}, {longitude}"));
        }

        context
    }

    /// Persist a conversation to SQLite in a background task.
    fn spawn_save_conversation(
        &self,
        run_id: &str,
        utterance: &str,
        is_vision: bool,
        history: &[Message],
        response_text: &str,
    ) {
        let db = self.db.clone();
        let run_id = run_id.to_string();
        let utterance = utterance.to_string();
        let history = history.to_vec();
        let response_text = response_text.to_string();

        tokio::spawn(async move {
            if let Err(e) = db
                .save_understand_conversation(
                    &run_id,
                    &utterance,
                    is_vision,
                    &history,
                    &response_text,
                )
                .await
            {
                warn!(error = %e, "failed to save conversation to db");
            }
        });
    }

    /// Call a configured agent with the given conversation context
    async fn evaluate_agent_conversation(
        &self,
        req: &SynapseUnderstandingRequest,
        run_id: &str,
        utterance: &str,
        history: &[Message],
        image: Option<Vec<u8>>,
        log_name: &str,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>,
        Status,
    > {
        let does_have_image = image.is_some();

        let templates = PromptTemplates {
            system_prompt: self.config.config.server.system_prompt.clone(),
            status_prompt: self.config.config.server.status_prompt.clone(),
        };

        let template_context = self.build_prompt_template_context(req, run_id, &self.config);
        let memory_context = if let Some(memory) = &self.memory {
            match memory.retrieve_context(utterance.to_string()).await {
                Ok(context) => context,
                Err(error) => {
                    warn!(error = %error, "memory retrieval failed");
                    None
                }
            }
        } else {
            None
        };

        let mut chat_request = LlmChatRequest::new(
            utterance.to_string(),
            history.to_vec(),
            templates,
            template_context,
            memory_context,
        );

        if let Some(image_bytes) = image {
            chat_request = chat_request.with_image(image_bytes);
        }

        match self.agent.chat(chat_request).await {
            Ok(ChatResult::Text(response_text)) => {
                info!(response = %response_text, "<<< {log_name} responding");
                self.spawn_save_conversation(
                    run_id,
                    utterance,
                    does_have_image,
                    history,
                    &response_text,
                );
                let response = SynapseUnderstandingResponse::action_response(
                    "Respond",
                    "I should respond to the user",
                    &serde_json::json!({"Response": response_text}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Ok(ChatResult::DeferredVision) => {
                info!("<<< LLM requested vision, returning UnderstandScene");
                let response = SynapseUnderstandingResponse::action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
            Err(error) => {
                warn!(error = %error, "LLM chat failed, falling back to error message");
                self.spawn_save_conversation(run_id, utterance, does_have_image, history, &error);
                let response = SynapseUnderstandingResponse::action_response(
                    "Respond",
                    "I encountered an error",
                    &serde_json::json!({"Response": error}).to_string(),
                    run_id,
                );
                Ok(Box::pin(tokio_stream::once(Ok(response))))
            }
        }
    }

    async fn understand_inner(
        &self,
        metadata: MetadataMap,
        req: SynapseUnderstandingRequest,
        log_name: &str,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>,
        Status,
    > {
        let utterance = &req.utterance;
        let run_id = extract_run_id(&metadata);

        info!(run_id = %run_id, utterance = %utterance, ">>> {log_name}");

        let (history, ctx) = if let Some(ref ctx) = req.device_context {
            info!(
                turns = ctx.turns.len(),
                is_locked = ctx.is_locked,
                location = %ctx.reverse_geocoded_location,
                "    device_context"
            );
            for (i, turn) in ctx.turns.iter().enumerate() {
                let kind = match &turn.content {
                    Some(synapse_chat_turn::Content::UserRequest(_)) => "user_request",
                    Some(synapse_chat_turn::Content::Action(a)) => {
                        debug!(idx = i, action = %a.action, input = %a.input, "    turn");
                        "action"
                    }
                    Some(synapse_chat_turn::Content::Observation(o)) => {
                        debug!(idx = i, is_final = o.is_final, action_name = %o.action_name, obs = %o.observation, "    turn");
                        "observation"
                    }
                    Some(synapse_chat_turn::Content::Message(_)) => "message",
                    Some(synapse_chat_turn::Content::End(_)) => "end",
                    Some(synapse_chat_turn::Content::Tao(_)) => "tao",
                    Some(synapse_chat_turn::Content::Interpretation(_)) => "interpretation",
                    Some(synapse_chat_turn::Content::Speech(_)) => "speech",
                    None => "empty",
                };
                debug!(idx = i, kind = kind, user = ?turn.user(), "    turn");
            }
            let h = extract_history(ctx, &self.image_store).await;
            if !h.is_empty() {
                info!(messages = h.len(), "    extracted history");
            }
            (h, Some(ctx))
        } else {
            (Vec::new(), None)
        };

        if let Some(ctx) = &ctx {
            // image_data attached inline by a device hook
            // This is only accessible if our modified hook code does this
            if let Some(image_bytes) = extract_most_recent_image_data(ctx) {
                info!(
                    image_bytes = image_bytes.len(),
                    "<<< Inline image data in Understand request, running chat with image"
                );
                return self
                    .evaluate_agent_conversation(
                        &req,
                        &run_id,
                        utterance,
                        &history,
                        Some(image_bytes),
                        log_name,
                    )
                    .await;
            }

            // A previous turn called AnalyzeImage and stored an image for us to retrieve in this step
            if let Some(image_bytes) = self.image_store.get_refresh(&run_id).await {
                info!(run_id = %run_id, image_bytes = image_bytes.len(), "<<< Have stored image for current run, running chat with image");
                return self
                    .evaluate_agent_conversation(
                        &req,
                        &run_id,
                        utterance,
                        &history,
                        Some(image_bytes),
                        log_name,
                    )
                    .await;
            }

            // Explicit vision request
            if is_vision_request(ctx) {
                info!("<<< Vision request detected, returning UnderstandScene");

                let response = SynapseUnderstandingResponse::action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    &run_id,
                );

                return Ok(Box::pin(tokio_stream::once(Ok(response))));
            }
        }

        // No images found in context, do a normal chat
        self.evaluate_agent_conversation(&req, &run_id, utterance, &history, None, log_name)
            .await
    }

    pub async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>>,
        Status,
    > {
        let metadata = request.metadata().clone();
        let req = request.into_inner();
        let stream = self.understand_inner(metadata, req, "Understand").await?;
        Ok(Response::new(stream))
    }

    pub async fn encrypted_understand(
        &self,
        request: Request<EncryptedSynapseUnderstandingRequest>,
    ) -> Result<
        Response<
            Pin<
                Box<
                    dyn Stream<Item = Result<EncryptedSynapseUnderstandingResponse, Status>> + Send,
                >,
            >,
        >,
        Status,
    > {
        let metadata = request.metadata().clone();
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let mut plain_req = SynapseUnderstandingRequest::decode(request_bytes).map_err(|e| {
            Status::invalid_argument(format!("bad SynapseUnderstandingRequest: {e}"))
        })?;

        if let Some(location_envelope) = req.location.as_ref() {
            if !location_envelope.data.is_empty() {
                let location =
                    encryption::LocationEnvelope::decode(location_envelope.data.as_slice())
                        .map_err(|e| {
                            Status::invalid_argument(format!("bad LocationEnvelope: {e}"))
                        })?;
                plain_req.location = Some(Location {
                    latitude: location.latitude as f64,
                    longitude: location.longitude as f64,
                });
            }
        }

        let plain_stream = self
            .understand_inner(metadata, plain_req, "EncryptedUnderstand")
            .await?;
        let encrypted_stream = plain_stream.map(|item| {
            item.map(|plain_response| EncryptedSynapseUnderstandingResponse {
                response: Some(EncryptedData::new(
                    "humane.aibus.SynapseUnderstandingResponse",
                    plain_response.encode_to_vec(),
                )),
            })
        });

        Ok(Response::new(Box::pin(encrypted_stream)))
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();

    if !value.is_empty() {
        Some(value.to_string())
    } else {
        None
    }
}

fn format_coordinate(value: f64) -> String {
    format!("{value:.3}")
}
