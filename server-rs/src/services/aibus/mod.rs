use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use self::completion::CompletionHandler;
use self::geolocate::GeoLocateHandler;
use self::nearby::NearbySearchHandler;
use self::reverse_geocode::ReverseGeocodeHandler;
use self::stubs::StubHandler;
use self::understand::UnderstandHandler;
use self::vision::VisionHandler;
use self::weather::WeatherHandler;
use crate::config::Config;
use crate::db::Database;
use crate::llm::LlmAgent;
use crate::nearby::NearbyClient;
use crate::proto::aibus::ai_bus_service_server::AiBusService;
use crate::proto::aibus::*;

mod completion;
mod envelope;
mod geolocate;
mod nearby;
mod reverse_geocode;
mod stubs;
mod understand;
mod vision;
mod weather;

pub struct AiBusServiceImpl {
    understand: UnderstandHandler,
    vision: VisionHandler,
    weather: WeatherHandler,
    nearby: NearbySearchHandler,
    reverse_geocode: ReverseGeocodeHandler,
    completion: CompletionHandler,
    geolocate: GeoLocateHandler,
    stubs: StubHandler,
}

impl AiBusServiceImpl {
    pub fn new(
        agent: Arc<RwLock<Arc<LlmAgent>>>,
        config: Arc<RwLock<Config>>,
        pirate_weather_api_key: Arc<RwLock<Option<String>>>,
        nearby_client: NearbyClient,
        http_client: reqwest::Client,
        db: Database,
    ) -> Self {
        Self {
            understand: UnderstandHandler::new(agent.clone(), config.clone(), db.clone()),
            vision: VisionHandler::new(agent.clone()),
            weather: WeatherHandler::new(http_client.clone(), pirate_weather_api_key.clone()),
            nearby: NearbySearchHandler::new(nearby_client),
            reverse_geocode: ReverseGeocodeHandler::new(http_client.clone()),
            completion: CompletionHandler::new(agent.clone(), config.clone()),
            geolocate: GeoLocateHandler,
            stubs: StubHandler,
        }
    }
}

#[tonic::async_trait]
impl AiBusService for AiBusServiceImpl {
    type UnderstandStream =
        Pin<Box<dyn Stream<Item = Result<SynapseUnderstandingResponse, Status>> + Send>>;
    type BidirectionalStreamingUnderstandStream =
        Pin<Box<dyn Stream<Item = Result<StreamingUnderstandResponse, Status>> + Send>>;
    type EncryptedStreamAIBusStream =
        Pin<Box<dyn Stream<Item = Result<EncryptedAiResponse, Status>> + Send>>;
    type EncryptedUnderstandStream =
        Pin<Box<dyn Stream<Item = Result<EncryptedSynapseUnderstandingResponse, Status>> + Send>>;

    async fn upload_file(
        &self,
        request: Request<UploadFileRequest>,
    ) -> Result<Response<UploadFileResponse>, Status> {
        self.stubs.upload_file(request).await
    }

    async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<Response<Self::UnderstandStream>, Status> {
        self.understand.understand(request).await
    }

    async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        self.vision.analyze_image(request).await
    }

    async fn function_execution(
        &self,
        request: Request<FunctionCall>,
    ) -> Result<Response<FunctionResponse>, Status> {
        self.stubs.function_execution(request).await
    }

    async fn server_stateful_understand(
        &self,
        request: Request<ServerStatefulUnderstandRequest>,
    ) -> Result<Response<ServerStatefulUnderstandResponse>, Status> {
        self.stubs.server_stateful_understand(request).await
    }

    async fn bidirectional_streaming_understand(
        &self,
        request: Request<tonic::Streaming<StreamingUnderstandRequest>>,
    ) -> Result<Response<Self::BidirectionalStreamingUnderstandStream>, Status> {
        self.stubs.bidirectional_streaming_understand(request).await
    }

    async fn encrypted_stream_ai_bus(
        &self,
        request: Request<tonic::Streaming<EncryptedAiRequest>>,
    ) -> Result<Response<Self::EncryptedStreamAIBusStream>, Status> {
        self.stubs.encrypted_stream_ai_bus(request).await
    }

    async fn encrypted_understand(
        &self,
        request: Request<EncryptedSynapseUnderstandingRequest>,
    ) -> Result<Response<Self::EncryptedUnderstandStream>, Status> {
        self.understand.encrypted_understand(request).await
    }

    async fn encrypted_loading_message(
        &self,
        request: Request<EncryptedLoadingMessageRequest>,
    ) -> Result<Response<EncryptedLoadingMessageResponse>, Status> {
        self.stubs.encrypted_loading_message(request).await
    }

    async fn encrypted_nearby_search(
        &self,
        request: Request<EncryptedNearbySearchRequest>,
    ) -> Result<Response<EncryptedNearbySearchResponse>, Status> {
        self.nearby.encrypted_nearby_search(request).await
    }

    async fn encrypted_navigation_directions(
        &self,
        request: Request<EncryptedNavigationDirectionsRequest>,
    ) -> Result<Response<EncryptedNavigationDirectionsResponse>, Status> {
        self.stubs.encrypted_navigation_directions(request).await
    }

    async fn encrypted_chat_completion(
        &self,
        request: Request<EncryptedChatCompletionRequest>,
    ) -> Result<Response<EncryptedChatCompletionResponse>, Status> {
        self.completion.encrypted_chat_completion(request).await
    }

    async fn encrypted_completion(
        &self,
        request: Request<EncryptedCompletionRequest>,
    ) -> Result<Response<EncryptedCompletionResponse>, Status> {
        self.completion.encrypted_completion(request).await
    }

    async fn encrypted_geo_locate(
        &self,
        request: Request<EncryptedGeoLocateRequest>,
    ) -> Result<Response<EncryptedGeoLocateResponse>, Status> {
        self.geolocate.encrypted_geo_locate(request).await
    }

    async fn encrypted_smart_playlist(
        &self,
        request: Request<EncryptedSmartPlaylistRequest>,
    ) -> Result<Response<EncryptedSmartPlaylistResponse>, Status> {
        self.stubs.encrypted_smart_playlist(request).await
    }

    async fn encrypted_weather(
        &self,
        request: Request<EncryptedWeatherRequest>,
    ) -> Result<Response<EncryptedWeatherResponse>, Status> {
        self.weather.encrypted_weather(request).await
    }

    async fn encrypted_reverse_geocode(
        &self,
        request: Request<EncryptedReverseGeocodeRequest>,
    ) -> Result<Response<EncryptedReverseGeocodeResponse>, Status> {
        self.reverse_geocode
            .encrypted_reverse_geocode(request)
            .await
    }

    async fn encrypted_function_execution(
        &self,
        request: Request<EncryptedFunctionCall>,
    ) -> Result<Response<EncryptedFunctionResponse>, Status> {
        self.stubs.encrypted_function_execution(request).await
    }

    async fn encrypted_analyze_image(
        &self,
        request: Request<EncryptedAnalyzeImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeImageResponse>, Status> {
        self.vision.encrypted_analyze_image(request).await
    }

    async fn encrypted_analyze_food_image(
        &self,
        request: Request<EncryptedAnalyzeFoodImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeFoodImageResponse>, Status> {
        self.stubs.encrypted_analyze_food_image(request).await
    }

    async fn encrypted_get_food_item(
        &self,
        request: Request<EncryptedGetFoodItemRequest>,
    ) -> Result<Response<EncryptedGetFoodItemResponse>, Status> {
        self.stubs.encrypted_get_food_item(request).await
    }

    async fn encrypted_action_based_interstitial(
        &self,
        request: Request<EncryptedActionBasedInterstitialRequest>,
    ) -> Result<Response<EncryptedActionBasedInterstitialResponse>, Status> {
        self.stubs
            .encrypted_action_based_interstitial(request)
            .await
    }

    async fn action_execution_test(
        &self,
        request: Request<ActionExecutionTestRequest>,
    ) -> Result<Response<ActionExecutionTestResponse>, Status> {
        self.stubs.action_execution_test(request).await
    }

    async fn transcription_repair_test(
        &self,
        request: Request<TranscriptionRepairTestRequest>,
    ) -> Result<Response<TranscriptionRepairTestResponse>, Status> {
        self.stubs.transcription_repair_test(request).await
    }

    async fn translate(
        &self,
        request: Request<EncryptedTranslateRequest>,
    ) -> Result<Response<EncryptedTranslateResponse>, Status> {
        self.stubs.translate(request).await
    }
}
