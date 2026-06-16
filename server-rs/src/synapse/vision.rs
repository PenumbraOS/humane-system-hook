use crate::proto::aibus::*;

/// Check if the current Understand request is a vision request.
pub fn is_vision_request(ctx: &SynapseDeviceContext) -> bool {
    for turn in ctx.turns.iter().rev() {
        if let Some(synapse_chat_turn::Content::UserRequest(req)) = &turn.content {
            return req.vision_requested
                == synapse_user_request_content::VisionRequested::Vision as i32;
        }
    }
    false
}

/// Extract raw image bytes from the latest UserRequest in the device context.
///
/// This image is only populated if the request is preceded by an AnalyzeImage call
pub fn extract_most_recent_image_data(ctx: &SynapseDeviceContext) -> Option<Vec<u8>> {
    for turn in ctx.turns.iter().rev() {
        if let Some(synapse_chat_turn::Content::UserRequest(req)) = &turn.content {
            if !req.image_data.is_empty() {
                return Some(req.image_data.clone());
            }
        }
    }

    None
}
