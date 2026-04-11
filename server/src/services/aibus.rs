use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use rig::completion::message::Message;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::llm::LlmAgent;
use crate::proto::aibus::ai_bus_service_server::AiBusService;
use crate::proto::aibus::*;

pub struct AiBusServiceImpl {
    pub agent: Arc<LlmAgent>,
}

/// Extract conversation history from device_context.turns into rig Messages.
///
/// Mapping (from GRPC_SERVICES.md):
///   user_request (USER)              → Message::user(request text)
///   action (ASSISTANT) "Respond"     → Message::assistant(response text from JSON)
///   message (ASSISTANT)              → Message::assistant(content)
///   message (SYSTEM)                 → Message::system(content)
///   Everything else                  → skipped (internal ReAct plumbing)
fn extract_history(ctx: &SynapseDeviceContext) -> Vec<Message> {
    let mut history = Vec::new();

    for turn in &ctx.turns {
        let user = turn.user(); // SynapseUser enum
        let content = match &turn.content {
            Some(c) => c,
            None => continue,
        };

        match content {
            synapse_chat_turn::Content::UserRequest(req) => {
                // Use repaired_request if available, otherwise the raw request
                let text = if !req.repaired_request.is_empty() {
                    &req.repaired_request
                } else {
                    &req.request
                };
                if !text.is_empty() {
                    debug!(text = %text, "  history: user_request");
                    history.push(Message::user(text));
                }
            }

            synapse_chat_turn::Content::Action(action) => {
                if action.action == "Respond" {
                    // Parse the response text from the JSON input field:
                    // {"Response": "actual text"}
                    if let Some(response_text) = extract_respond_text(&action.input) {
                        if !response_text.is_empty() {
                            debug!(text = %response_text, "  history: action(Respond)");
                            history.push(Message::assistant(response_text));
                        }
                    }
                }
                // Non-Respond actions (SearchWeb, UnderstandScene, etc.) are
                // internal ReAct tool calls — skip for LLM context.
            }

            synapse_chat_turn::Content::Message(msg) => {
                if !msg.content.is_empty() {
                    match user {
                        SynapseUser::Assistant => {
                            debug!(text = %msg.content, "  history: message(assistant)");
                            history.push(Message::assistant(&msg.content));
                        }
                        SynapseUser::System => {
                            debug!(text = %msg.content, "  history: message(system)");
                            history.push(Message::system(&msg.content));
                        }
                        _ => {
                            // USER messages as message content are unusual, treat as user
                            debug!(text = %msg.content, "  history: message(user)");
                            history.push(Message::user(&msg.content));
                        }
                    }
                }
            }

            // Observation, tao, interpretation, end, speech — skip
            _ => {}
        }
    }

    history
}

/// Parse the Response text from a Respond action's JSON input.
/// Expected format: {"Response": "some text"}
fn extract_respond_text(input: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(input).ok()?;
    parsed.get("Response")?.as_str().map(|s| s.to_string())
}

/// Check if the current Understand request is a vision request.
fn is_vision_request(ctx: &SynapseDeviceContext) -> bool {
    for turn in ctx.turns.iter().rev() {
        if let Some(synapse_chat_turn::Content::UserRequest(req)) = &turn.content {
            return req.vision_requested
                == synapse_user_request_content::VisionRequested::Vision as i32;
        }
    }
    false
}

/// Extract the observation text from a completed UnderstandScene round-trip.
fn extract_vision_observation(ctx: &SynapseDeviceContext) -> Option<String> {
    for turn in ctx.turns.iter().rev() {
        if let Some(synapse_chat_turn::Content::Observation(obs)) = &turn.content {
            if !obs.is_final && !obs.observation.trim().is_empty() {
                return Some(obs.observation.trim().to_string());
            }
        }
    }
    None
}

/// Build a single SynapseUnderstandingResponse containing an action turn.
fn make_action_response(
    action_name: &str,
    thought: &str,
    input_json: &str,
    parent_id: &str,
) -> SynapseUnderstandingResponse {
    let turn_id = Uuid::new_v4().to_string();

    let action = SynapseActionContent {
        thought: thought.into(),
        action: action_name.into(),
        input: input_json.into(),
        device_payload: Vec::new(),
        source: SynapseSource::Server as i32,
    };

    let turn = SynapseChatTurn {
        user: SynapseUser::Assistant as i32,
        timestamp: None,
        identifier: turn_id,
        parent_identifier: parent_id.into(),
        content: Some(synapse_chat_turn::Content::Action(action)),
    };

    SynapseUnderstandingResponse {
        response: String::new(),
        is_final: false,
        body: Some(synapse_understanding_response::Body::Turn(turn)),
    }
}

#[tonic::async_trait]
impl AiBusService for AiBusServiceImpl {
    type UnderstandStream =
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>;

    async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<Response<Self::UnderstandStream>, Status> {
        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let utterance = &req.utterance;
        let run_id = metadata
            .get("x-ai-mic-run-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        info!(run_id = %run_id, utterance = %utterance, ">>> Understand");

        // Extract conversation history from device context
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
            let h = extract_history(ctx);
            if !h.is_empty() {
                info!(messages = h.len(), "    extracted history");
            }
            (h, Some(ctx))
        } else {
            (Vec::new(), None)
        };

        if let Some(ref loc) = req.location {
            info!(lat = loc.latitude, lon = loc.longitude, "    location");
        }

        // --- Vision handling ---
        //
        // Phase 1: If this is a fresh vision request (vision_requested == VISION),
        // return an UnderstandScene action to trigger camera capture + AnalyzeImage.
        //
        // Phase 2: If the turns contain a non-final observation for UnderstandScene
        // (the round-trip from AnalyzeImage is complete), echo the observation text
        // back as a Respond action.
        if let Some(ctx) = ctx {
            // Check for phase 2 first: observation already present from AnalyzeImage
            if let Some(observation_text) = extract_vision_observation(ctx) {
                info!(observation = %observation_text, "<<< Vision round-trip complete, responding");

                // TODO: We probably want to feed the observation + original question
                // back to the LLM for a more conversational/contextualized response,
                // rather than just echoing the raw observation. For now, keep it simple.
                let response = make_action_response(
                    "Respond",
                    "I analyzed the image and should share my observation with the user",
                    &serde_json::json!({"Response": observation_text}).to_string(),
                    &run_id,
                );

                let stream = tokio_stream::once(Ok(response));
                return Ok(Response::new(Box::pin(stream)));
            }

            // Check for phase 1: fresh vision request, no observation yet
            if is_vision_request(ctx) {
                info!("<<< Vision request detected, returning UnderstandScene");

                let response = make_action_response(
                    "UnderstandScene",
                    "I should look at what the user is seeing",
                    &serde_json::json!({"Question": utterance}).to_string(),
                    &run_id,
                );

                let stream = tokio_stream::once(Ok(response));
                return Ok(Response::new(Box::pin(stream)));
            }
        }

        // --- Normal (non-vision) handling ---

        // Call LLM agent with conversation history
        let response_text = match self.agent.chat(utterance, history).await {
            Ok(text) => text,
            Err(e) => {
                warn!(error = %e, "LLM chat failed, falling back to error message");
                format!("Sorry, I encountered an error: {}", e)
            }
        };

        info!(response = %response_text, "<<< Understand responding");

        let response = make_action_response(
            "Respond",
            "I should respond to the user",
            &serde_json::json!({"Response": response_text}).to_string(),
            &run_id,
        );

        // Stream a single response then complete
        let stream = tokio_stream::once(Ok(response));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        let req = request.into_inner();

        let question = if !req.request.is_empty() {
            &req.request
        } else if !req.utterance.is_empty() {
            &req.utterance
        } else {
            "What do you see in this image?"
        };

        let image_bytes = &req.image_data;
        info!(
            question = %question,
            image_bytes = image_bytes.len(),
            hints = ?req.image_hints,
            ">>> AnalyzeImage"
        );

        if image_bytes.is_empty() {
            warn!("AnalyzeImage called with empty image_data");
            return Err(Status::invalid_argument("image_data is empty"));
        }

        // Encode raw JPEG bytes to base64 for the LLM vision API
        let image_b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);

        let observation = match self.agent.vision_prompt(question, &image_b64).await {
            Ok(text) => text,
            Err(e) => {
                warn!(error = %e, "Vision LLM failed");
                format!("I wasn't able to analyze the image: {}", e)
            }
        };

        info!(observation = %observation, "<<< AnalyzeImage responding");

        let response = AnalyzeImageResponse {
            observation: String::new(),
            nested_analyze_image_response: Some(NestedAnalyzeImageResponse {
                response_one_of: Some(
                    nested_analyze_image_response::ResponseOneOf::GenericImageResponse(
                        GenericImageResponse { observation },
                    ),
                ),
            }),
        };

        Ok(Response::new(response))
    }
}
