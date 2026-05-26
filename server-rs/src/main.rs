//! Standalone server for Humane AI Pin.
//!
//! Serves gRPC services and an HTTP upload endpoint on the same port.
//! gRPC requests (content-type: application/grpc) are routed to tonic;
//! HTTP PUT /upload/:uuid/:filename is handled by axum for media uploads.

mod api;
mod config;
mod db;
mod dedup;
mod esim;
mod llm;
mod nearby;
mod services;
mod storage;
mod synapse;

/// Generated protobuf/gRPC modules.
#[allow(unused)]
mod proto {
    pub mod aibus {
        tonic::include_proto!("humane.aibus");
    }
    pub mod pushrelay {
        tonic::include_proto!("humane.pushrelay");
    }
    pub mod featureflags {
        tonic::include_proto!("humane.featureflags");
    }
    pub mod account {
        tonic::include_proto!("humane.account");
    }
    pub mod contacts {
        tonic::include_proto!("humane.contacts");
    }
    pub mod events {
        tonic::include_proto!("humane.events");
    }
    pub mod provisioning {
        tonic::include_proto!("humane.provisioning");
    }
    pub mod capture {
        tonic::include_proto!("humane.capture");
    }
    pub mod partnerservices {
        tonic::include_proto!("humane.partnerservices");
    }
    pub mod common {
        pub mod encryption {
            tonic::include_proto!("humane.common.encryption");
        }
    }
    pub mod privacy {
        pub mod common {
            tonic::include_proto!("humane.privacy.grpc.common");
        }
        pub mod pub_ {
            tonic::include_proto!("humane.privacy.grpc.r#pub");
        }
    }
}

use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::put;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;

use proto::account::user_information_service_server::UserInformationServiceServer;
use proto::account::wifi_config_service_server::WifiConfigServiceServer;
use proto::aibus::ai_bus_service_server::AiBusServiceServer;
use proto::capture::capture_service_server::CaptureServiceServer;
use proto::contacts::contacts_rpc_service_server::ContactsRpcServiceServer;
use proto::events::events_ingest_service_server::EventsIngestServiceServer;
use proto::featureflags::feature_flags_service_server::FeatureFlagsServiceServer;
use proto::partnerservices::partner_token_rpc_service_server::PartnerTokenRpcServiceServer;
use proto::privacy::pub_::public_privacy_service_server::PublicPrivacyServiceServer;
use proto::provisioning::device_onboarding_dac_service_server::DeviceOnboardingDacServiceServer;
use proto::pushrelay::push_relay_service_server::PushRelayServiceServer;

use services::aibus::AiBusServiceImpl;
use services::capture::CaptureServiceImpl;
use services::contacts::ContactsRpcServiceImpl;
use services::events::EventsIngestServiceImpl;
use services::featureflags::FeatureFlagsServiceImpl;
use services::partnerservices::PartnerServicesImpl;
use services::privacy::PublicPrivacyServiceImpl;
use services::provisioning::{OnboardingCa, ProvisioningServiceImpl};
use services::pushrelay::PushRelayServiceImpl;
use services::user_info::UserInformationServiceImpl;
use services::wifi_config::WifiConfigServiceImpl;

use config::Config;
use db::Database;
use dedup::DedupRouter;
use llm::LlmAgent;
use storage::MediaStore;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use std::time::Duration;

use crate::api::device::DeviceVersionCollector;

#[cfg(not(target_os = "android"))]
fn load_dotenv(config_path: &FsPath) {
    let Some(config_dir) = config_path.parent() else {
        return;
    };

    let dotenv_path = config_dir.join(".env");
    if !dotenv_path.exists() {
        return;
    }

    match dotenvy::from_path(&dotenv_path) {
        Ok(()) => info!(path = %dotenv_path.display(), "loaded .env file"),
        Err(error) => warn!(path = %dotenv_path.display(), %error, "failed to load .env file"),
    }
}

#[cfg(target_os = "android")]
fn load_dotenv(_config_path: &FsPath) {}

// ─── HTTP upload handler ────────────────────────────────────────────

/// Shared state passed to the axum upload handler.
#[derive(Clone)]
struct UploadState {
    store: Arc<Mutex<MediaStore>>,
}

/// PUT /upload/:uuid/:filename — receives media file bytes from the device.
async fn upload_handler(
    Path((uuid, filename)): Path<(String, String)>,
    State(state): State<UploadState>,
    body: Body,
) -> impl IntoResponse {
    info!(uuid, filename, "<<< HTTP PUT /upload");

    // Read the full body
    let bytes = match axum::body::to_bytes(body, 256 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to read upload body");
            return (StatusCode::BAD_REQUEST, format!("failed to read body: {e}"));
        }
    };

    info!(uuid, filename, bytes = bytes.len(), "upload received");

    let store = state.store.lock().await;

    // Ensure the directory exists (create "unknown" bucket if needed)
    let dir = store.base_dir().join(&uuid);
    if !dir.exists() {
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::error!(error = %e, "failed to create upload dir");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create dir: {e}"),
            );
        }
    }

    match store.save_upload(&uuid, &filename, &bytes).await {
        Ok(()) => (StatusCode::CREATED, "OK".to_string()),
        Err(e) => {
            tracing::error!(uuid, filename, error = %e, "upload save failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("save failed: {e}"),
            )
        }
    }
}

/// Catches any request that doesn't match a registered HTTP or gRPC route.
/// Logs a warning and returns HTTP 404.
async fn fallback_handler(request: axum::extract::Request) -> impl IntoResponse {
    warn!(
        method = %request.method(),
        path = %request.uri(),
        content_type = request.headers().get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none"),
        "unhandled request. No matching route"
    );
    (StatusCode::NOT_FOUND, "not found")
}

/// Middleware that inspects gRPC responses for UNIMPLEMENTED status (code 12)
/// and logs a warning when one is detected.
async fn log_grpc_unimplemented(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;

    // gRPC status code 12 = UNIMPLEMENTED.
    // Tonic sets this in the `grpc-status` header for routing-level rejections.
    if let Some(status) = response.headers().get("grpc-status") {
        if status.as_bytes() == b"12" {
            warn!(path = %path, "gRPC UNIMPLEMENTED. method not registered");
        }
    }

    response
}

// ─── main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Locate config file: check --config <path>, then ./config.toml, then next to binary
    let config_path = std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    load_dotenv(&config_path);

    let config = Config::load(&config_path)?;

    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    // Optional rolling file appender, used both for persistence and for the
    // `/api/logs/server` REST endpoint. The guard must outlive the program;
    // we leak it intentionally.
    let file_layer = if let Some(dir) = config.logging.log_dir.as_deref() {
        match std::fs::create_dir_all(dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::Builder::new()
                    .rotation(tracing_appender::rolling::Rotation::DAILY)
                    .filename_prefix(&config.logging.file_prefix)
                    .max_log_files(config.logging.max_files)
                    .build(dir)
                    .map_err(|e| format!("failed to build rolling log appender: {e}"))?;
                let (nb, guard) = tracing_appender::non_blocking(appender);
                Box::leak(Box::new(guard));
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(nb)
                        .with_ansi(false)
                        .with_target(true),
                )
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to create log_dir {:?}: {}. file logging disabled",
                    dir, e
                );
                None
            }
        }
    } else {
        None
    };

    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        #[cfg(target_os = "android")]
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .compact()
            .without_time();
        #[cfg(not(target_os = "android"))]
        let stdout_layer = tracing_subscriber::fmt::layer();

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    }

    let http_client = reqwest::Client::builder().tls_backend_native().build()?;

    // Build LLM agent (behind RwLock for hot-reload)
    let agent = Arc::new(LlmAgent::from_config(&config.llm, http_client.clone())?);
    let shared_agent = Arc::new(RwLock::new(agent));

    // Resolve PirateWeather API key for weather service (behind RwLock for hot-reload)
    let pirate_weather_api_key = config.weather.resolve_api_key();
    let has_weather_key = pirate_weather_api_key.is_some();
    let shared_weather_key = Arc::new(RwLock::new(pirate_weather_api_key));

    // Generate ephemeral CA for signing DUC certificates during onboarding
    let onboarding_ca = Arc::new(OnboardingCa::generate()?);
    let user_id = uuid::Uuid::new_v4().to_string();
    let display_name = config
        .server
        .display_name
        .clone()
        .unwrap_or_else(|| "Penumbra".into());

    // Open SQLite database
    let database = Database::open(&config.storage.db_path)?;

    // Open media store (uses SQLite for metadata, filesystem for binary files)
    let media_store = Arc::new(Mutex::new(
        MediaStore::open(&config.storage.media_dir, database.clone()).await?,
    ));

    // Broadcast channel for real-time events to web portal clients
    let (events_tx, _) = tokio::sync::broadcast::channel::<api::Event>(256);

    let http_bind_addr: std::net::SocketAddr = config.server.http_bind_addr.parse()?;
    let grpc_bind_addr: std::net::SocketAddr = config.server.grpc_bind_addr.parse()?;
    let public_addr = config.server.public_addr.clone();

    let provider_label = config.llm.provider.as_str().to_uppercase();
    info!("============================================================");
    info!("Humane HTTP server listening on {}", http_bind_addr);
    info!(
        "Humane gRPC server listening on {} (plaintext)",
        grpc_bind_addr
    );
    info!("Upload URL base: http://{}/upload/", public_addr);
    info!(
        "LLM provider: {} (model: {})",
        provider_label, config.llm.model
    );
    info!(
        "Onboarding: display_name={}, user_id={}",
        display_name, user_id
    );
    if has_weather_key {
        info!("Weather: PirateWeather API key configured");
    } else {
        info!("Weather: no API key. EncryptedWeather will return UNAVAILABLE");
    }
    info!(
        "Storage: media_dir={}, db={}",
        config.storage.media_dir, config.storage.db_path
    );
    info!("Services:");
    info!(
        "  - humane.aibus.AIBusService/Understand       ({})",
        config.llm.provider
    );
    info!(
        "  - humane.aibus.AIBusService/AnalyzeImage     ({}, vision)",
        config.llm.provider
    );
    info!("  - humane.pushrelay.PushRelayService/Subscribe (push stream)");
    info!("  - humane.pushrelay.PushRelayService/GetPushTokens (empty)");
    info!("  - humane.featureflags.FeatureFlagsService/GetFlags (empty)");
    info!("  - humane.account.WifiConfigService/ListSecureWifiConfigs (empty)");
    info!("  - humane.account.UserInformationService/GetUserPersonalDetails (stub)");
    info!("  - humane.contacts.ContactsRPCService/GetContacts (sqlite contacts)");
    info!("  - humane.events.EventsIngestService/Ingest (discard)");
    info!("  - humane.events.EventsIngestService/IngestBatch (discard)");
    info!("  - humane.provisioning.DeviceOnboardingDACService/* (onboarding)");
    info!("  - humane.capture.CaptureService/* (photo/video/note storage)");
    info!("  - humane.aibus.AIBusService/EncryptedUnderstand (LLM)");
    info!("  - humane.aibus.AIBusService/EncryptedAnalyzeImage (vision)");
    info!("  - humane.aibus.AIBusService/EncryptedCompletion (LLM)");
    info!("  - humane.aibus.AIBusService/EncryptedChatCompletion (LLM)");
    info!("  - humane.aibus.AIBusService/EncryptedNearbySearch (Overpass/OSM)");
    info!("  - humane.aibus.AIBusService/EncryptedReverseGeocode (Nominatim/OSM)");
    info!("  - humane.aibus.AIBusService/EncryptedGeoLocate (stub: device GPS preferred)");
    info!("  - humane.privacy.grpc.pub.PublicPrivacyService/* (stub — empty responses)");
    info!("  - PUT /upload/:uuid/:filename (HTTP media upload)");
    info!("  - GET /api/* (REST API for web portal)");
    info!("  - GET /api/events (NDJSON event stream)");
    info!("  - GET /api/logs/server (rolling log file dump)");
    info!("  - GET /api/logs/logcat (Android only)");
    info!("  - All other RPCs: UNIMPLEMENTED");
    info!("============================================================");

    type AiBus = AiBusServiceServer<AiBusServiceImpl>;

    // Shared config for hot-reload via the web portal
    let shared_config = Arc::new(RwLock::new(config.clone()));

    // Capture logging settings for the API state before `config` is moved.
    let log_dir_for_api: Option<PathBuf> = config.logging.log_dir.as_ref().map(PathBuf::from);
    let log_file_prefix_for_api: String = config.logging.file_prefix.clone();

    // Build the gRPC service stack as a native axum::Router.
    let grpc_router = DedupRouter::new(AiBusServiceServer::new(AiBusServiceImpl::new(
        shared_agent.clone(),
        shared_config.clone(),
        shared_weather_key.clone(),
        nearby::NearbyClient::new(http_client.clone()),
        http_client.clone(),
        database.clone(),
    )))
    .dedup::<AiBus>("EncryptedWeather", Duration::from_secs(300))
    .dedup::<AiBus>("EncryptedNearbySearch", Duration::from_secs(30))
    .dedup::<AiBus>("EncryptedReverseGeocode", Duration::from_secs(30))
    .dedup::<AiBus>("EncryptedUnderstand", Duration::from_millis(200))
    .dedup::<AiBus>("EncryptedAnalyzeImage", Duration::from_millis(200))
    .dedup::<AiBus>("EncryptedCompletion", Duration::from_millis(200))
    .dedup::<AiBus>("EncryptedChatCompletion", Duration::from_millis(200))
    .dedup::<AiBus>("Understand", Duration::from_millis(200))
    .dedup::<AiBus>("AnalyzeImage", Duration::from_millis(200))
    .add_service(PushRelayServiceServer::new(PushRelayServiceImpl))
    .add_service(FeatureFlagsServiceServer::new(FeatureFlagsServiceImpl))
    .add_service(WifiConfigServiceServer::new(WifiConfigServiceImpl))
    .add_service(UserInformationServiceServer::new(
        UserInformationServiceImpl,
    ))
    .add_service(ContactsRpcServiceServer::new(ContactsRpcServiceImpl {
        db: database.clone(),
    }))
    .add_service(EventsIngestServiceServer::new(EventsIngestServiceImpl))
    .add_service(DeviceOnboardingDacServiceServer::new(
        ProvisioningServiceImpl {
            ca: onboarding_ca,
            display_name,
            user_id,
        },
    ))
    .add_service(CaptureServiceServer::new(CaptureServiceImpl {
        store: media_store.clone(),
        server_addr: public_addr.clone(),
        events_tx: events_tx.clone(),
    }))
    .add_service(PublicPrivacyServiceServer::new(PublicPrivacyServiceImpl))
    .add_service(PartnerTokenRpcServiceServer::new(PartnerServicesImpl))
    .into_axum_router()
    .fallback(fallback_handler)
    .layer(axum::middleware::from_fn(log_grpc_unimplemented));

    // Build the axum HTTP router for upload endpoint
    let upload_state = UploadState {
        store: media_store.clone(),
    };

    let esim_bridge = esim::EsimBridge::start();
    let device_versions = DeviceVersionCollector::collect().await;

    // Build the REST API router for the web portal
    let api_state = api::ApiState {
        store: media_store,
        db: database,
        events_tx,
        config_path,
        shared_config,
        shared_agent,
        http_client: http_client.clone(),
        shared_weather_key,
        log_dir: log_dir_for_api,
        log_file_prefix: log_file_prefix_for_api,
        esim_bridge,
        contact_client_reset_pending: Arc::new(AtomicBool::new(false)),
        device_versions,
    };

    // CORS layer for the web portal (public HTTPS → local HTTP via LNA).
    // The `Access-Control-Allow-Local-Network` header is required by the
    // Local Network Access spec for the browser to allow cross-origin
    // requests from a public site to a LAN server.
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([http::header::CONTENT_TYPE])
        .expose_headers([http::header::CONTENT_TYPE]);

    let api_router = api::router(api_state)
        .layer(cors)
        .layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async {
                let mut response = next.run(request).await;
                // Inject the LNA header on every response (including preflights).
                response.headers_mut().insert(
                    HeaderName::from_static("access-control-allow-local-network"),
                    HeaderValue::from_static("true"),
                );
                // Also advertise in preflight Allow-Headers so the browser accepts it.
                response.headers_mut().insert(
                    HeaderName::from_static("access-control-allow-private-network"),
                    HeaderValue::from_static("true"),
                );
                response
            },
        ));

    // Apply trace layer to the HTTP router.
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(|request: &http::Request<axum::body::Body>| {
            tracing::info_span!(
                "req",
                method = %request.method(),
                path = %request.uri().path(),
            )
        })
        .on_request(
            |_request: &http::Request<axum::body::Body>, _span: &tracing::Span| {
                info!("request");
            },
        )
        .on_response(
            |response: &http::Response<_>, latency: std::time::Duration, _span: &tracing::Span| {
                info!(latency = ?latency, status = %response.status(), "response");
            },
        )
        .on_failure(
            |error: tower_http::classify::ServerErrorsFailureClass,
             latency: std::time::Duration,
             _span: &tracing::Span| {
                tracing::error!(latency = ?latency, error = %error, "failed");
            },
        );

    let http_app = axum::Router::new()
        .route("/upload/{uuid}/{filename}", put(upload_handler))
        .with_state(upload_state)
        .merge(api_router)
        .fallback(fallback_handler)
        .layer(trace_layer);

    let http_listener = tokio::net::TcpListener::bind(http_bind_addr).await?;
    let grpc_listener = tokio::net::TcpListener::bind(grpc_bind_addr).await?;

    let http_server = axum::serve(http_listener, http_app);
    let grpc_server = axum::serve(grpc_listener, grpc_router);

    tokio::try_join!(http_server, grpc_server)?;

    Ok(())
}
