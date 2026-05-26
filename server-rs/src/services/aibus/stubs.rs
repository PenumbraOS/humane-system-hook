use std::pin::Pin;

use prost::Message as _;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::proto::{aibus::*, common::encryption::EncryptedData};

#[derive(Default)]
pub struct StubHandler;

impl StubHandler {
    pub async fn upload_file(
        &self,
        _request: Request<UploadFileRequest>,
    ) -> Result<Response<UploadFileResponse>, Status> {
        info!(">>> UploadFile (stub)");
        Ok(Response::new(UploadFileResponse {}))
    }

    pub async fn function_execution(
        &self,
        _request: Request<FunctionCall>,
    ) -> Result<Response<FunctionResponse>, Status> {
        info!(">>> FunctionExecution (stub)");
        Ok(Response::new(FunctionResponse {}))
    }

    pub async fn server_stateful_understand(
        &self,
        _request: Request<ServerStatefulUnderstandRequest>,
    ) -> Result<Response<ServerStatefulUnderstandResponse>, Status> {
        info!(">>> ServerStatefulUnderstand (stub)");
        Ok(Response::new(ServerStatefulUnderstandResponse {}))
    }

    pub async fn bidirectional_streaming_understand(
        &self,
        _request: Request<tonic::Streaming<StreamingUnderstandRequest>>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<StreamingUnderstandResponse, Status>> + Send>>>,
        Status,
    > {
        info!(">>> BidirectionalStreamingUnderstand (stub)");
        Ok(Response::new(Box::pin(tokio_stream::empty())))
    }

    pub async fn encrypted_stream_ai_bus(
        &self,
        _request: Request<tonic::Streaming<EncryptedAiRequest>>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<EncryptedAiResponse, Status>> + Send>>>,
        Status,
    > {
        info!(">>> EncryptedStreamAIBus (stub)");
        Ok(Response::new(Box::pin(tokio_stream::empty())))
    }

    pub async fn encrypted_loading_message(
        &self,
        _request: Request<EncryptedLoadingMessageRequest>,
    ) -> Result<Response<EncryptedLoadingMessageResponse>, Status> {
        info!(">>> EncryptedLoadingMessage (stub)");
        Ok(Response::new(EncryptedLoadingMessageResponse {
            response: Some(EncryptedData::stub("humane.aibus.LoadingMessageResponse")),
        }))
    }

    pub async fn encrypted_navigation_directions(
        &self,
        _request: Request<EncryptedNavigationDirectionsRequest>,
    ) -> Result<Response<EncryptedNavigationDirectionsResponse>, Status> {
        info!(">>> EncryptedNavigationDirections (stub)");
        Ok(Response::new(EncryptedNavigationDirectionsResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.NavigationDirectionsResponse",
                NavigationDirectionsResponse::default().encode_to_vec(),
            )),
        }))
    }

    pub async fn encrypted_smart_playlist(
        &self,
        _request: Request<EncryptedSmartPlaylistRequest>,
    ) -> Result<Response<EncryptedSmartPlaylistResponse>, Status> {
        info!(">>> EncryptedSmartPlaylist (stub)");
        Ok(Response::new(EncryptedSmartPlaylistResponse {
            response: Some(EncryptedData::stub("humane.aibus.SmartPlaylistResponse")),
        }))
    }

    pub async fn encrypted_function_execution(
        &self,
        _request: Request<EncryptedFunctionCall>,
    ) -> Result<Response<EncryptedFunctionResponse>, Status> {
        info!(">>> EncryptedFunctionExecution (stub)");
        Ok(Response::new(EncryptedFunctionResponse {
            response: Some(EncryptedData::new(
                "humane.aibus.FunctionResponse",
                FunctionResponse::default().encode_to_vec(),
            )),
        }))
    }

    pub async fn encrypted_analyze_food_image(
        &self,
        _request: Request<EncryptedAnalyzeFoodImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeFoodImageResponse>, Status> {
        info!(">>> EncryptedAnalyzeFoodImage (stub)");
        Ok(Response::new(EncryptedAnalyzeFoodImageResponse {
            response: Some(EncryptedData::stub("humane.aibus.AnalyzeFoodImageResponse")),
        }))
    }

    pub async fn encrypted_get_food_item(
        &self,
        _request: Request<EncryptedGetFoodItemRequest>,
    ) -> Result<Response<EncryptedGetFoodItemResponse>, Status> {
        info!(">>> EncryptedGetFoodItem (stub)");
        Ok(Response::new(EncryptedGetFoodItemResponse {
            response: Some(EncryptedData::stub("humane.aibus.GetFoodItemResponse")),
        }))
    }

    pub async fn encrypted_action_based_interstitial(
        &self,
        _request: Request<EncryptedActionBasedInterstitialRequest>,
    ) -> Result<Response<EncryptedActionBasedInterstitialResponse>, Status> {
        info!(">>> EncryptedActionBasedInterstitial (stub)");
        Ok(Response::new(EncryptedActionBasedInterstitialResponse {
            response: Some(EncryptedData::stub(
                "humane.aibus.ActionBasedInterstitialResponse",
            )),
        }))
    }

    pub async fn action_execution_test(
        &self,
        _request: Request<ActionExecutionTestRequest>,
    ) -> Result<Response<ActionExecutionTestResponse>, Status> {
        info!(">>> ActionExecutionTest (stub)");
        Ok(Response::new(ActionExecutionTestResponse {}))
    }

    pub async fn transcription_repair_test(
        &self,
        _request: Request<TranscriptionRepairTestRequest>,
    ) -> Result<Response<TranscriptionRepairTestResponse>, Status> {
        info!(">>> TranscriptionRepairTest (stub)");
        Ok(Response::new(TranscriptionRepairTestResponse {}))
    }

    pub async fn translate(
        &self,
        _request: Request<EncryptedTranslateRequest>,
    ) -> Result<Response<EncryptedTranslateResponse>, Status> {
        info!(">>> Translate (stub)");
        Ok(Response::new(EncryptedTranslateResponse {
            response: Some(EncryptedData::stub("humane.aibus.TranslateResponse")),
        }))
    }
}
