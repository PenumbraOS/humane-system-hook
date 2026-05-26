use std::sync::Arc;

use base64::Engine as _;
use prost::Message as _;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::llm::LlmAgent;
use crate::proto::aibus::*;
use crate::proto::common::encryption::EncryptedData;

pub struct VisionHandler {
    agent: Arc<RwLock<Arc<LlmAgent>>>,
}

impl VisionHandler {
    pub fn new(agent: Arc<RwLock<Arc<LlmAgent>>>) -> Self {
        Self { agent }
    }

    #[allow(deprecated)]
    async fn analyze_image_inner(
        &self,
        req: AnalyzeImageRequest,
    ) -> Result<AnalyzeImageResponse, Status> {
        let question = if !req.request.is_empty() {
            &req.request
        } else if !req.utterance.is_empty() {
            &req.utterance
        } else {
            "What do you see in this image?"
        };

        let decoded_image;
        let image_bytes = if !req.image_data.is_empty() {
            req.image_data.as_slice()
        } else if !req.base64_encoded_image.is_empty() {
            decoded_image = base64::engine::general_purpose::STANDARD
                .decode(&req.base64_encoded_image)
                .map_err(|e| Status::invalid_argument(format!("bad base64 image: {e}")))?;
            decoded_image.as_slice()
        } else {
            &[]
        };

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

        let image_b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);

        let agent = self.agent.read().await.clone();
        let observation = match agent.vision_prompt(question, &image_b64).await {
            Ok(text) => text,
            Err(error) => {
                warn!(error = %error, "Vision LLM failed");
                error
            }
        };

        info!(observation = %observation, "<<< AnalyzeImage responding");

        Ok(AnalyzeImageResponse {
            observation: String::new(),
            nested_analyze_image_response: Some(NestedAnalyzeImageResponse {
                response_one_of: Some(
                    nested_analyze_image_response::ResponseOneOf::GenericImageResponse(
                        GenericImageResponse { observation },
                    ),
                ),
            }),
        })
    }

    pub async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        let response = self.analyze_image_inner(request.into_inner()).await?;
        Ok(Response::new(response))
    }

    pub async fn encrypted_analyze_image(
        &self,
        request: Request<EncryptedAnalyzeImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeImageResponse>, Status> {
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let image_req = AnalyzeImageRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad AnalyzeImageRequest: {e}")))?;
        let image_response = self.analyze_image_inner(image_req).await?;

        Ok(Response::new(EncryptedAnalyzeImageResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.AnalyzeImageResponse",
                image_response.encode_to_vec(),
            )),
        }))
    }
}
