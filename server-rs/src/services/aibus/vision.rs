use base64::Engine as _;
use prost::Message as _;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::envelope::unwrap_plaintext_data;
use crate::proto::aibus::*;
use crate::proto::common::encryption::EncryptedData;
use crate::synapse::extract_run_id;
use crate::synapse::image_store::LiveImageStore;

pub struct VisionHandler {
    image_store: LiveImageStore,
}

impl VisionHandler {
    pub fn new(image_store: LiveImageStore) -> Self {
        Self { image_store }
    }

    #[allow(deprecated)]
    async fn analyze_image_inner(
        &self,
        run_id: &str,
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
            run_id = %run_id,
            question = %question,
            image_bytes = image_bytes.len(),
            hints = ?req.image_hints,
            ">>> AnalyzeImage"
        );

        if image_bytes.is_empty() {
            warn!("AnalyzeImage called with empty image_data");
            return Err(Status::invalid_argument("image_data is empty"));
        }

        // Cache the captured image so a future Understand call can retrieve it and pass it to a LLM
        self.image_store.put(run_id, image_bytes.to_vec()).await;

        info!(run_id = %run_id, "<<< AnalyzeImage stored image");

        Ok(AnalyzeImageResponse {
            observation: String::new(),
            nested_analyze_image_response: Some(NestedAnalyzeImageResponse {
                response_one_of: Some(
                    nested_analyze_image_response::ResponseOneOf::GenericImageResponse(
                        GenericImageResponse {
                            // The client doesn't consume this response, so we just return a placeholder string
                            observation: "Image captured".to_string(),
                        },
                    ),
                ),
            }),
        })
    }

    pub async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        let run_id = extract_run_id(request.metadata());
        let response = self
            .analyze_image_inner(&run_id, request.into_inner())
            .await?;
        Ok(Response::new(response))
    }

    pub async fn encrypted_analyze_image(
        &self,
        request: Request<EncryptedAnalyzeImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeImageResponse>, Status> {
        let run_id = extract_run_id(request.metadata());
        let req = request.into_inner();
        let request_bytes = unwrap_plaintext_data(&req.request)?;
        let image_req = AnalyzeImageRequest::decode(request_bytes)
            .map_err(|e| Status::invalid_argument(format!("bad AnalyzeImageRequest: {e}")))?;
        let image_response = self.analyze_image_inner(&run_id, image_req).await?;

        Ok(Response::new(EncryptedAnalyzeImageResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.AnalyzeImageResponse",
                image_response.encode_to_vec(),
            )),
        }))
    }
}
