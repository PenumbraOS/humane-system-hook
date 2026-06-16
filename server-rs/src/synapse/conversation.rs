use base64::Engine;
use rig::completion::message::{ImageMediaType, Message, UserContent};
use rig::OneOrMany;
use tracing::debug;

use crate::proto::aibus::*;
use crate::synapse::image_store::LiveImageStore;

/// Extract conversation history from device_context.turns into rig Messages,
/// reconstructing each prior image-bearing user request as a single multimodal
/// `Message::User` (text + image) keyed 1:1 to that turn's run-id
pub async fn extract_history(
    ctx: &SynapseDeviceContext,
    image_store: &LiveImageStore,
) -> Vec<Message> {
    let mut history = Vec::new();

    let last_user_request_idx = ctx
        .turns
        .iter()
        .rposition(|t| matches!(&t.content, Some(synapse_chat_turn::Content::UserRequest(_))));

    for (i, turn) in ctx.turns.iter().enumerate() {
        // Skip the current run's user_request
        if Some(i) == last_user_request_idx {
            continue;
        }

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

                if text.is_empty() {
                    continue;
                }

                // Resolve an image for this message, if any
                let image_bytes = if !req.image_data.is_empty() {
                    Some(req.image_data.clone())
                } else if !turn.identifier.is_empty() {
                    image_store.get_refresh(&turn.identifier).await
                } else {
                    None
                };

                match image_bytes {
                    Some(bytes) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let content = OneOrMany::many(vec![
                            UserContent::text(text),
                            UserContent::image_base64(b64, Some(ImageMediaType::JPEG), None),
                        ])
                        .expect("non-empty content vec");
                        history.push(Message::User { content });
                    }
                    None => {
                        debug!(text = %text, "  history: user_request");
                        history.push(Message::user(text));
                    }
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
