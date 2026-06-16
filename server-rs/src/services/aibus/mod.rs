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
use crate::config::ResolvedConfig;
use crate::db::Database;
use crate::llm::memory::MemoryService;
use crate::llm::LlmAgent;
use crate::nearby::NearbyClient;
use crate::proto::aibus::ai_bus_service_server::AiBusService;
use crate::proto::aibus::*;
use crate::synapse::image_store::LiveImageStore;

mod completion;
mod envelope;
mod geolocate;
mod nearby;
mod reverse_geocode;
mod stubs;
mod understand;
mod vision;
mod weather;

#[derive(Clone)]
pub struct AiBus {
    handlers: Arc<RwLock<Arc<AiBusHanders>>>,
}

impl AiBus {
    pub fn new(
        agent: Arc<LlmAgent>,
        config: Arc<ResolvedConfig>,
        nearby_client: NearbyClient,
        http_client: reqwest::Client,
        db: Database,
        memory: Option<MemoryService>,
    ) -> Self {
        Self {
            handlers: Arc::new(RwLock::new(Arc::new(AiBusHanders::new(
                agent,
                config,
                nearby_client,
                http_client,
                db,
                memory,
            )))),
        }
    }

    pub async fn replace(&self, next: Arc<AiBusHanders>) {
        let mut current = self.handlers.write().await;
        *current = next;
    }

    async fn handlers(&self) -> Arc<AiBusHanders> {
        self.handlers.read().await.clone()
    }
}

#[tonic::async_trait]
impl AiBusService for AiBus {
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
        self.handlers().await.stubs.upload_file(request).await
    }

    async fn understand(
        &self,
        request: Request<SynapseUnderstandingRequest>,
    ) -> Result<Response<Self::UnderstandStream>, Status> {
        self.handlers().await.understand.understand(request).await
    }

    async fn analyze_image(
        &self,
        request: Request<AnalyzeImageRequest>,
    ) -> Result<Response<AnalyzeImageResponse>, Status> {
        self.handlers().await.vision.analyze_image(request).await
    }

    async fn function_execution(
        &self,
        request: Request<FunctionCall>,
    ) -> Result<Response<FunctionResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .function_execution(request)
            .await
    }

    async fn server_stateful_understand(
        &self,
        request: Request<ServerStatefulUnderstandRequest>,
    ) -> Result<Response<ServerStatefulUnderstandResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .server_stateful_understand(request)
            .await
    }

    async fn bidirectional_streaming_understand(
        &self,
        request: Request<tonic::Streaming<StreamingUnderstandRequest>>,
    ) -> Result<Response<Self::BidirectionalStreamingUnderstandStream>, Status> {
        self.handlers()
            .await
            .stubs
            .bidirectional_streaming_understand(request)
            .await
    }

    async fn encrypted_stream_ai_bus(
        &self,
        request: Request<tonic::Streaming<EncryptedAiRequest>>,
    ) -> Result<Response<Self::EncryptedStreamAIBusStream>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_stream_ai_bus(request)
            .await
    }

    async fn encrypted_understand(
        &self,
        request: Request<EncryptedSynapseUnderstandingRequest>,
    ) -> Result<Response<Self::EncryptedUnderstandStream>, Status> {
        self.handlers()
            .await
            .understand
            .encrypted_understand(request)
            .await
    }

    async fn encrypted_loading_message(
        &self,
        request: Request<EncryptedLoadingMessageRequest>,
    ) -> Result<Response<EncryptedLoadingMessageResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_loading_message(request)
            .await
    }

    async fn encrypted_nearby_search(
        &self,
        request: Request<EncryptedNearbySearchRequest>,
    ) -> Result<Response<EncryptedNearbySearchResponse>, Status> {
        self.handlers()
            .await
            .nearby
            .encrypted_nearby_search(request)
            .await
    }

    async fn encrypted_navigation_directions(
        &self,
        request: Request<EncryptedNavigationDirectionsRequest>,
    ) -> Result<Response<EncryptedNavigationDirectionsResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_navigation_directions(request)
            .await
    }

    async fn encrypted_chat_completion(
        &self,
        request: Request<EncryptedChatCompletionRequest>,
    ) -> Result<Response<EncryptedChatCompletionResponse>, Status> {
        self.handlers()
            .await
            .completion
            .encrypted_chat_completion(request)
            .await
    }

    async fn encrypted_completion(
        &self,
        request: Request<EncryptedCompletionRequest>,
    ) -> Result<Response<EncryptedCompletionResponse>, Status> {
        self.handlers()
            .await
            .completion
            .encrypted_completion(request)
            .await
    }

    async fn encrypted_geo_locate(
        &self,
        request: Request<EncryptedGeoLocateRequest>,
    ) -> Result<Response<EncryptedGeoLocateResponse>, Status> {
        self.handlers()
            .await
            .geolocate
            .encrypted_geo_locate(request)
            .await
    }

    async fn encrypted_smart_playlist(
        &self,
        request: Request<EncryptedSmartPlaylistRequest>,
    ) -> Result<Response<EncryptedSmartPlaylistResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_smart_playlist(request)
            .await
    }

    async fn encrypted_weather(
        &self,
        request: Request<EncryptedWeatherRequest>,
    ) -> Result<Response<EncryptedWeatherResponse>, Status> {
        self.handlers()
            .await
            .weather
            .encrypted_weather(request)
            .await
    }

    async fn encrypted_reverse_geocode(
        &self,
        request: Request<EncryptedReverseGeocodeRequest>,
    ) -> Result<Response<EncryptedReverseGeocodeResponse>, Status> {
        self.handlers()
            .await
            .reverse_geocode
            .encrypted_reverse_geocode(request)
            .await
    }

    async fn encrypted_function_execution(
        &self,
        request: Request<EncryptedFunctionCall>,
    ) -> Result<Response<EncryptedFunctionResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_function_execution(request)
            .await
    }

    async fn encrypted_analyze_image(
        &self,
        request: Request<EncryptedAnalyzeImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeImageResponse>, Status> {
        self.handlers()
            .await
            .vision
            .encrypted_analyze_image(request)
            .await
    }

    async fn encrypted_analyze_food_image(
        &self,
        request: Request<EncryptedAnalyzeFoodImageRequest>,
    ) -> Result<Response<EncryptedAnalyzeFoodImageResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_analyze_food_image(request)
            .await
    }

    async fn encrypted_get_food_item(
        &self,
        request: Request<EncryptedGetFoodItemRequest>,
    ) -> Result<Response<EncryptedGetFoodItemResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_get_food_item(request)
            .await
    }

    async fn encrypted_action_based_interstitial(
        &self,
        request: Request<EncryptedActionBasedInterstitialRequest>,
    ) -> Result<Response<EncryptedActionBasedInterstitialResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .encrypted_action_based_interstitial(request)
            .await
    }

    async fn action_execution_test(
        &self,
        request: Request<ActionExecutionTestRequest>,
    ) -> Result<Response<ActionExecutionTestResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .action_execution_test(request)
            .await
    }

    async fn transcription_repair_test(
        &self,
        request: Request<TranscriptionRepairTestRequest>,
    ) -> Result<Response<TranscriptionRepairTestResponse>, Status> {
        self.handlers()
            .await
            .stubs
            .transcription_repair_test(request)
            .await
    }

    async fn translate(
        &self,
        request: Request<EncryptedTranslateRequest>,
    ) -> Result<Response<EncryptedTranslateResponse>, Status> {
        self.handlers().await.stubs.translate(request).await
    }
}

pub struct AiBusHanders {
    understand: UnderstandHandler,
    vision: VisionHandler,
    weather: WeatherHandler,
    nearby: NearbySearchHandler,
    reverse_geocode: ReverseGeocodeHandler,
    completion: CompletionHandler,
    geolocate: GeoLocateHandler,
    stubs: StubHandler,
}

impl AiBusHanders {
    pub fn new(
        agent: Arc<LlmAgent>,
        config: Arc<ResolvedConfig>,
        nearby_client: NearbyClient,
        http_client: reqwest::Client,
        db: Database,
        memory: Option<MemoryService>,
    ) -> Self {
        let live_image_store = LiveImageStore::new();

        Self {
            understand: UnderstandHandler::new(
                agent.clone(),
                config.clone(),
                db.clone(),
                memory.clone(),
                live_image_store.clone(),
            ),
            vision: VisionHandler::new(live_image_store),
            weather: WeatherHandler::new(
                http_client.clone(),
                config.pirate_weather_api_key.clone(),
            ),
            nearby: NearbySearchHandler::new(nearby_client),
            reverse_geocode: ReverseGeocodeHandler::new(http_client.clone()),
            completion: CompletionHandler::new(agent.clone(), config.clone(), memory),
            geolocate: GeoLocateHandler,
            stubs: StubHandler,
        }
    }
}
