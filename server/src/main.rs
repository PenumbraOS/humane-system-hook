//! Standalone server for Humane AI Pin.
//!
//! Serves gRPC services and an HTTP upload endpoint on the same port.
//! gRPC requests (content-type: application/grpc) are routed to tonic;
//! HTTP PUT /upload/:uuid/:filename is handled by axum for media uploads.

mod config;
mod dedup;
mod llm;
mod nearby;
mod services;
mod storage;

/// Generated protobuf/gRPC modules.
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

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::put;
use tokio::sync::Mutex;

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
use dedup::DedupRouter;
use llm::LlmAgent;
use storage::MediaStore;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use std::time::Duration;

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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Locate config file: check --config <path>, then ./config.toml, then next to binary
    let config_path = std::env::args()
        .position(|a| a == "--config")
        .and_then(|i| std::env::args().nth(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let config = Config::load(&config_path)?;

    // CLI port arg overrides config (for backward compat: `humane-server 9090`)
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(config.server.port);

    // Build LLM agent
    let agent = Arc::new(LlmAgent::from_config(
        &config.llm,
        &config.server.system_prompt,
    )?);

    // Resolve PirateWeather API key for weather service
    let pirate_weather_api_key = config.weather.resolve_api_key();
    let http_client = reqwest::Client::new();

    // Generate ephemeral CA for signing DUC certificates during onboarding
    let onboarding_ca = Arc::new(OnboardingCa::generate()?);
    let user_id = uuid::Uuid::new_v4().to_string();
    let display_name = config
        .server
        .display_name
        .clone()
        .unwrap_or_else(|| "Penumbra".into());

    // Open media store
    let media_store = Arc::new(Mutex::new(
        MediaStore::open(&config.storage.media_dir).await?,
    ));

    // Server address the device will use in upload URLs
    let server_addr = format!("0.0.0.0:{port}");
    let bind_addr: std::net::SocketAddr = server_addr.parse()?;
    let public_addr = config
        .server
        .public_addr
        .clone()
        .unwrap_or_else(|| format!("{}", bind_addr));

    let provider_label = config.llm.provider.to_uppercase();
    info!("============================================================");
    info!("Humane gRPC server listening on {} (plaintext)", bind_addr);
    info!("Upload URL base: http://{}/upload/", public_addr);
    info!(
        "LLM provider: {} (model: {})",
        provider_label, config.llm.model
    );
    info!(
        "Onboarding: display_name={}, user_id={}",
        display_name, user_id
    );
    if pirate_weather_api_key.is_some() {
        info!("Weather: PirateWeather API key configured");
    } else {
        info!("Weather: no API key. EncryptedWeather will return UNAVAILABLE");
    }
    info!("Storage: media_dir={}", config.storage.media_dir);
    info!("Services:");
    info!(
        "  - humane.aibus.AIBusService/Understand       ({})",
        config.llm.provider
    );
    info!(
        "  - humane.aibus.AIBusService/AnalyzeImage     ({}, vision)",
        config.llm.provider
    );
    info!("  - humane.pushrelay.PushRelayService/Subscribe (no-op hold)");
    info!("  - humane.pushrelay.PushRelayService/GetPushTokens (empty)");
    info!("  - humane.featureflags.FeatureFlagsService/GetFlags (empty)");
    info!("  - humane.account.WifiConfigService/ListSecureWifiConfigs (empty)");
    info!("  - humane.account.UserInformationService/GetUserPersonalDetails (stub)");
    info!("  - humane.contacts.ContactsRPCService/GetContacts (empty)");
    info!("  - humane.events.EventsIngestService/Ingest (discard)");
    info!("  - humane.events.EventsIngestService/IngestBatch (discard)");
    info!("  - humane.provisioning.DeviceOnboardingDACService/* (onboarding)");
    info!("  - humane.capture.CaptureService/* (photo/video/note storage)");
    info!("  - humane.aibus.AIBusService/EncryptedNearbySearch (Overpass/OSM)");
    info!("  - humane.privacy.grpc.pub.PublicPrivacyService/* (stub — empty responses)");
    info!("  - PUT /upload/:uuid/:filename (HTTP media upload)");
    info!("  - All other RPCs: UNIMPLEMENTED");
    info!("============================================================");

    type AiBus = AiBusServiceServer<AiBusServiceImpl>;

    // Build the gRPC service stack as a native axum::Router.
    let grpc_router = DedupRouter::new(AiBusServiceServer::new(AiBusServiceImpl {
        agent,
        pirate_weather_api_key,
        nearby_client: nearby::NearbyClient::new(http_client.clone()),
        http_client,
    }))
    .dedup::<AiBus>("EncryptedWeather", Duration::from_secs(300))
    .dedup::<AiBus>("EncryptedNearbySearch", Duration::from_secs(30))
    .dedup::<AiBus>("Understand", Duration::from_millis(200))
    .dedup::<AiBus>("AnalyzeImage", Duration::from_millis(200))
    .add_service(PushRelayServiceServer::new(PushRelayServiceImpl))
    .add_service(FeatureFlagsServiceServer::new(FeatureFlagsServiceImpl))
    .add_service(WifiConfigServiceServer::new(WifiConfigServiceImpl))
    .add_service(UserInformationServiceServer::new(
        UserInformationServiceImpl,
    ))
    .add_service(ContactsRpcServiceServer::new(ContactsRpcServiceImpl))
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
    }))
    .add_service(PublicPrivacyServiceServer::new(PublicPrivacyServiceImpl))
    .add_service(PartnerTokenRpcServiceServer::new(PartnerServicesImpl))
    .into_axum_router()
    .layer(axum::middleware::from_fn(log_grpc_unimplemented));

    // Build the axum HTTP router for upload endpoint
    let upload_state = UploadState { store: media_store };

    // Apply trace layer to the combined router.
    // Use new_for_http() (not new_for_grpc()) since we serve both HTTP uploads
    // and gRPC. The HTTP classifier only flags 5xx as failures, which is
    // correct: gRPC responses always have HTTP 200 status.
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

    // Explicit HTTP routes get priority; gRPC routes are merged in.
    // Since gRPC paths (e.g. /humane.aibus.AIBusService/Understand) don't
    // conflict with /upload/{uuid}/{filename}, merge works cleanly.
    // The fallback handler catches any request that doesn't match a known route.
    let app = axum::Router::new()
        .route("/upload/{uuid}/{filename}", put(upload_handler))
        .with_state(upload_state)
        .merge(grpc_router)
        .fallback(fallback_handler)
        .layer(trace_layer);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
