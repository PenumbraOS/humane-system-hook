//! REST/JSON API for the web portal.
//!
//! These endpoints are consumed by the Pin Center web app over the Local
//! Network Access (LNA) API.  All responses include CORS headers so the
//! public HTTPS portal can reach this HTTP server on the LAN.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::config::Config;
use crate::esim::{CellularStatusError, DeviceToggleError, EsimBridge, EsimRequestError, EsimRequestRecord, EsimSnapshot};
use crate::llm::LlmAgent;
use crate::storage::{MediaStore, MemoryRecord};

const ESIM_GETTER_TIMEOUT: Duration = Duration::from_secs(20);
const CELLULAR_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const NETWORK_TOGGLE_TIMEOUT: Duration = Duration::from_secs(10);

// ─── Shared state ───────────────────────────────────────────────────

/// State shared across all API handlers.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<MediaStore>>,
    pub config: Arc<Config>,
    pub events_tx: tokio::sync::broadcast::Sender<Event>,
    /// Path to config.toml on disk — needed for writing settings back.
    pub config_path: PathBuf,
    /// Live config that can be updated at runtime.
    pub shared_config: Arc<RwLock<Config>>,
    /// Hot-swappable LLM agent (shared with AiBusServiceImpl).
    pub shared_agent: Arc<RwLock<Arc<LlmAgent>>>,
    /// Shared HTTP client for outbound requests.
    pub http_client: HttpClient,
    /// Hot-swappable weather API key (shared with AiBusServiceImpl).
    pub shared_weather_key: Arc<RwLock<Option<String>>>,
    /// Directory where rolling log files are written, if file logging is enabled.
    pub log_dir: Option<PathBuf>,
    /// File-name prefix for rolling log files.
    pub log_file_prefix: String,
    /// Persistent eSIM bridge to the Android server app.
    pub esim_bridge: EsimBridge,
}

// ─── Event types for the streaming endpoint ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    MemoryCreated { memory: MemoryRecord },
    MemoryCompleted { uuid: String },
    MemoryDeleted { uuid: String },
    Heartbeat,
}

// ─── Router ─────────────────────────────────────────────────────────

/// Build the `/api/*` router.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/memories", get(list_memories))
        .route("/api/memories/{uuid}", get(get_memory))
        .route("/api/memories/{uuid}", delete(delete_memory))
        .route(
            "/api/memories/{uuid}/thumbnail/{index}",
            get(get_thumbnail),
        )
        .route(
            "/api/memories/{uuid}/files/{filename}",
            get(get_file),
        )
        .route("/api/device", get(get_device))
        .route("/api/settings", get(get_settings))
        .route("/api/settings", put(update_settings))
        .route("/api/events", get(event_stream))
        .route("/api/cellular/service-status", get(get_cellular_service_status))
        .route("/api/cellular/set-enabled", put(set_cellular_enabled))
        .route("/api/wifi/set-enabled", put(set_wifi_enabled))
        .route("/api/logs/server", get(get_server_logs))
        .route("/api/logs/logcat", get(get_logcat_logs))
        .route("/api/esim/state", get(get_esim_state))
        .route("/api/esim/events", get(esim_event_stream))
        .route("/api/esim/requests/{request_id}", get(get_esim_request))
        .route("/api/esim/get-profiles", put(esim_get_profiles))
        .route("/api/esim/get-active-profile", put(esim_get_active_profile))
        .route("/api/esim/get-active-iccid", put(esim_get_active_iccid))
        .route("/api/esim/get-eid", put(esim_get_eid))
        .route("/api/esim/enable-profile", put(esim_enable_profile))
        .route("/api/esim/disable-profile", put(esim_disable_profile))
        .route("/api/esim/set-nickname", put(esim_set_nickname))
        .route("/api/esim/delete-profile", put(esim_delete_profile))
        .route("/api/esim/download-verify-enable", put(esim_download_verify_enable))
        .with_state(state)
}

// ─── Health ─────────────────────────────────────────────────────────

async fn health(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let config = state.shared_config.read().await;
    let name = config
        .server
        .display_name
        .clone()
        .unwrap_or_else(|| "Penumbra".into());

    Json(serde_json::json!({
        "status": "ok",
        "name": name,
        "version": env!("PENUMBRA_VERSION"),
    }))
}

// ─── Memories ───────────────────────────────────────────────────────

async fn list_memories(State(state): State<ApiState>) -> Json<Vec<MemoryRecord>> {
    let store = state.store.lock().await;
    Json(store.list_memories().await)
}

async fn get_memory(
    Path(uuid): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<MemoryRecord>, StatusCode> {
    let store = state.store.lock().await;
    match store.get_memory(&uuid).await {
        Some(record) => Ok(Json(record)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn delete_memory(
    Path(uuid): Path<String>,
    State(state): State<ApiState>,
) -> Result<StatusCode, StatusCode> {
    let mut store = state.store.lock().await;
    match store.delete_memory(&uuid).await {
        Ok(true) => {
            let _ = state.events_tx.send(Event::MemoryDeleted {
                uuid: uuid.clone(),
            });
            info!(uuid, "memory deleted via API");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(uuid, error = %e, "failed to delete memory");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// ─── File serving ───────────────────────────────────────────────────

async fn get_thumbnail(
    Path((uuid, index)): Path<(String, usize)>,
    State(state): State<ApiState>,
) -> Result<Response, StatusCode> {
    let store = state.store.lock().await;
    let filename = format!("thumbnail_{index}.jpg");
    let path = store.base_dir().join(&uuid).join(&filename);
    drop(store);

    serve_file(&path, "image/jpeg").await
}

async fn get_file(
    Path((uuid, filename)): Path<(String, String)>,
    State(state): State<ApiState>,
) -> Result<Response, StatusCode> {
    let store = state.store.lock().await;
    let path = store.base_dir().join(&uuid).join(&filename);
    drop(store);

    let content_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    serve_file(&path, &content_type).await
}

async fn serve_file(path: &std::path::Path, content_type: &str) -> Result<Response, StatusCode> {
    let data = tokio::fs::read(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            tracing::error!(path = %path.display(), error = %e, "failed to read file");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    Ok(([(header::CONTENT_TYPE, content_type.to_string())], data).into_response())
}

// ─── Device info ────────────────────────────────────────────────────

#[derive(Serialize)]
struct DeviceInfo {
    display_name: String,
    http_bind_addr: String,
    grpc_bind_addr: String,
    llm_provider: String,
    llm_model: String,
}

async fn get_device(State(state): State<ApiState>) -> Json<DeviceInfo> {
    let config = state.shared_config.read().await;
    Json(DeviceInfo {
        display_name: config
            .server
            .display_name
            .clone()
            .unwrap_or_else(|| "Penumbra".into()),
        http_bind_addr: config.server.http_bind_addr.clone(),
        grpc_bind_addr: config.server.grpc_bind_addr.clone(),
        llm_provider: config.llm.provider.clone(),
        llm_model: config.llm.model.clone(),
    })
}

// ─── Settings ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsResponse {
    llm: LlmSettingsResponse,
    server: ServerSettingsResponse,
    storage: StorageSettingsResponse,
    weather: WeatherSettingsResponse,
}

#[derive(Serialize)]
struct LlmSettingsResponse {
    provider: String,
    model: String,
    has_api_key: bool,
    base_url: Option<String>,
    gemini_google_search: bool,
}

#[derive(Serialize)]
struct ServerSettingsResponse {
    http_bind_addr: String,
    grpc_bind_addr: String,
    public_addr: String,
    system_prompt: String,
    display_name: Option<String>,
}

#[derive(Serialize)]
struct StorageSettingsResponse {
    media_dir: String,
    db_path: String,
}

#[derive(Serialize)]
struct WeatherSettingsResponse {
    has_api_key: bool,
}

async fn get_settings(State(state): State<ApiState>) -> Json<SettingsResponse> {
    let config = state.shared_config.read().await;
    Json(SettingsResponse {
        llm: LlmSettingsResponse {
            provider: config.llm.provider.clone(),
            model: config.llm.model.clone(),
            has_api_key: config.llm.resolve_api_key().is_some(),
            base_url: config.llm.base_url.clone(),
            gemini_google_search: config.llm.gemini_google_search,
        },
        server: ServerSettingsResponse {
            http_bind_addr: config.server.http_bind_addr.clone(),
            grpc_bind_addr: config.server.grpc_bind_addr.clone(),
            public_addr: config.server.public_addr.clone(),
            system_prompt: config.server.system_prompt.clone(),
            display_name: config.server.display_name.clone(),
        },
        storage: StorageSettingsResponse {
            media_dir: config.storage.media_dir.clone(),
            db_path: config.storage.db_path.clone(),
        },
        weather: WeatherSettingsResponse {
            has_api_key: config.weather.resolve_api_key().is_some(),
        },
    })
}

#[derive(Deserialize)]
struct EsimIccidRequest {
    iccid: String,
}

#[derive(Deserialize)]
struct EsimNicknameRequest {
    iccid: String,
    nickname: String,
}

#[derive(Deserialize)]
struct EsimActivationCodeRequest {
    activation_code: String,
}

#[derive(Serialize)]
struct EsimRequestAcceptedResponse {
    request_id: String,
}

#[derive(Deserialize)]
struct SetEnabledRequest {
    enabled: bool,
}

async fn get_esim_state(State(state): State<ApiState>) -> Json<EsimSnapshot> {
    Json(state.esim_bridge.snapshot().await)
}

async fn get_cellular_service_status(State(state): State<ApiState>) -> Response {
    match state.esim_bridge.get_cellular_status(CELLULAR_STATUS_TIMEOUT).await {
        Ok(event) => Json(event).into_response(),
        Err(error) => {
            warn!(error = ?error, "failed to fetch cellular service status");
            cellular_status_error_response(error)
        }
    }
}

async fn set_cellular_enabled(
    State(state): State<ApiState>,
    Json(body): Json<SetEnabledRequest>,
) -> Response {
    match state.esim_bridge.set_cellular_enabled(body.enabled, NETWORK_TOGGLE_TIMEOUT).await {
        Ok(event) => Json(event).into_response(),
        Err(error) => {
            warn!(error = ?error, enabled = body.enabled, "failed to toggle cellular data");
            device_toggle_error_response(error)
        }
    }
}

async fn set_wifi_enabled(
    State(state): State<ApiState>,
    Json(body): Json<SetEnabledRequest>,
) -> Response {
    match state.esim_bridge.set_wifi_enabled(body.enabled, NETWORK_TOGGLE_TIMEOUT).await {
        Ok(event) => Json(event).into_response(),
        Err(error) => {
            warn!(error = ?error, enabled = body.enabled, "failed to toggle Wi-Fi");
            device_toggle_error_response(error)
        }
    }
}

async fn get_esim_request(
    Path(request_id): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<EsimRequestRecord>, StatusCode> {
    state
        .esim_bridge
        .get_request(&request_id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn esim_get_profiles(State(state): State<ApiState>) -> Response {
    submit_esim_request_and_wait(
        &state,
        "humane.connectivity.esimlpa.getProfiles",
        serde_json::json!({}),
        &["esim.profiles_result"],
    )
    .await
}

async fn esim_get_active_profile(State(state): State<ApiState>) -> Response {
    submit_esim_request_and_wait(
        &state,
        "humane.connectivity.esimlpa.getActiveProfile",
        serde_json::json!({}),
        &["esim.active_profile_result"],
    )
    .await
}

async fn esim_get_active_iccid(State(state): State<ApiState>) -> Response {
    submit_esim_request_and_wait(
        &state,
        "humane.connectivity.esimlpa.getActiveprofileICCID",
        serde_json::json!({}),
        &["esim.active_iccid_result"],
    )
    .await
}

async fn esim_get_eid(State(state): State<ApiState>) -> Response {
    submit_esim_request_and_wait(
        &state,
        "humane.connectivity.esimlpa.getEID",
        serde_json::json!({}),
        &["esim.device_identifiers_result"],
    )
    .await
}

async fn esim_enable_profile(
    State(state): State<ApiState>,
    Json(body): Json<EsimIccidRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.enableProfile",
        serde_json::json!({ "iccid": body.iccid }),
    ).await
}

async fn esim_disable_profile(
    State(state): State<ApiState>,
    Json(body): Json<EsimIccidRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.disableProfile",
        serde_json::json!({ "iccid": body.iccid }),
    ).await
}

async fn esim_set_nickname(
    State(state): State<ApiState>,
    Json(body): Json<EsimNicknameRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.setNickname",
        serde_json::json!({ "iccid": body.iccid, "nickname": body.nickname }),
    ).await
}

async fn esim_delete_profile(
    State(state): State<ApiState>,
    Json(body): Json<EsimIccidRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.deleteProfile",
        serde_json::json!({ "iccid": body.iccid }),
    ).await
}

async fn esim_download_verify_enable(
    State(state): State<ApiState>,
    Json(body): Json<EsimActivationCodeRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.downloadVerifyAndEnableProfile",
        serde_json::json!({ "activationCode": body.activation_code }),
    ).await
}

async fn submit_esim_request(
    state: &ApiState,
    action: &str,
    payload: serde_json::Value,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    match state.esim_bridge.submit_request(action.to_string(), payload).await {
        Ok(request_id) => Ok(Json(EsimRequestAcceptedResponse { request_id })),
        Err(error) => {
            warn!(%error, action, "failed to submit eSIM request");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn submit_esim_request_and_wait(
    state: &ApiState,
    action: &str,
    payload: serde_json::Value,
    terminal_types: &[&str],
) -> Response {
    match state
        .esim_bridge
        .submit_request_and_wait(
            action.to_string(),
            payload,
            terminal_types,
            ESIM_GETTER_TIMEOUT,
        )
        .await
    {
        Ok(event) => Json(event).into_response(),
        Err(error) => {
            warn!(error = ?error, action, "failed to complete synchronous eSIM request");
            esim_request_error_response(error)
        }
    }
}

fn esim_request_error_response(error: EsimRequestError) -> Response {
    match error {
        EsimRequestError::BridgeError { event, .. } => {
            (StatusCode::BAD_GATEWAY, Json(event)).into_response()
        }
        EsimRequestError::Timeout { request_id } => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "type": "esim.request_timeout",
                "request_id": request_id,
                "payload": {
                    "message": "timed out waiting for terminal event"
                }
            })),
        )
            .into_response(),
        EsimRequestError::Internal { request_id, message } => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "type": "esim.bridge_error",
                "request_id": request_id,
                "payload": {
                    "message": message
                }
            })),
        )
            .into_response(),
    }
}

fn cellular_status_error_response(error: CellularStatusError) -> Response {
    match error {
        CellularStatusError::BridgeError(event) => (StatusCode::BAD_GATEWAY, Json(event)).into_response(),
        CellularStatusError::Timeout { request_id } => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "type": "cellular.status_timeout",
                "request_id": request_id,
                "payload": {
                    "message": "timed out waiting for cellular status"
                }
            })),
        )
            .into_response(),
        CellularStatusError::Internal(message) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "type": "cellular.status_error",
                "payload": {
                    "message": message
                }
            })),
        )
            .into_response(),
    }
}

fn device_toggle_error_response(error: DeviceToggleError) -> Response {
    match error {
        DeviceToggleError::BridgeError(event) => (StatusCode::BAD_GATEWAY, Json(event)).into_response(),
        DeviceToggleError::Timeout { request_id } => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "type": "device.toggle_timeout",
                "request_id": request_id,
                "payload": {
                    "message": "timed out waiting for toggle result"
                }
            })),
        )
            .into_response(),
        DeviceToggleError::Internal(message) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "type": "device.toggle_error",
                "payload": {
                    "message": message
                }
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateSettingsRequest {
    llm: Option<UpdateLlmSettings>,
    server: Option<UpdateServerSettings>,
    weather: Option<UpdateWeatherSettings>,
    /// Storage is read-only; presence in the request is rejected.
    storage: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UpdateLlmSettings {
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    gemini_google_search: Option<bool>,
}

#[derive(Deserialize)]
struct UpdateServerSettings {
    /// Read-only — rejected if present.
    http_bind_addr: Option<serde_json::Value>,
    /// Read-only — rejected if present.
    grpc_bind_addr: Option<serde_json::Value>,
    /// Read-only — rejected if present.
    public_addr: Option<serde_json::Value>,
    system_prompt: Option<String>,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct UpdateWeatherSettings {
    pirate_weather_api_key: Option<String>,
}

async fn update_settings(
    State(state): State<ApiState>,
    Json(body): Json<UpdateSettingsRequest>,
) -> Response {
    // Reject attempts to change read-only fields.
    if let Some(ref server) = body.server {
        if server.http_bind_addr.is_some() {
            return (
                StatusCode::BAD_REQUEST,
                "http_bind_addr cannot be changed at runtime (requires server restart)",
            )
                .into_response();
        }
        if server.grpc_bind_addr.is_some() {
            return (
                StatusCode::BAD_REQUEST,
                "grpc_bind_addr cannot be changed at runtime (requires server restart)",
            )
                .into_response();
        }
        if server.public_addr.is_some() {
            return (
                StatusCode::BAD_REQUEST,
                "public_addr cannot be changed at runtime (requires server restart)",
            )
                .into_response();
        }
    }
    if body.storage.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "storage paths cannot be changed at runtime (requires server restart)",
        )
            .into_response();
    }

    // Take a write lock on the shared config and apply changes.
    let mut config = state.shared_config.write().await;

    let mut llm_changed = false;
    let mut system_prompt_changed = false;

    // --- LLM changes ---
    if let Some(ref llm) = body.llm {
        if let Some(ref provider) = llm.provider {
            if *provider != config.llm.provider {
                config.llm.provider = provider.clone();
                llm_changed = true;
            }
        }
        if let Some(ref model) = llm.model {
            if *model != config.llm.model {
                config.llm.model = model.clone();
                llm_changed = true;
            }
        }
        if let Some(ref api_key) = llm.api_key {
            config.llm.api_key = if api_key.is_empty() {
                None
            } else {
                Some(api_key.clone())
            };
            llm_changed = true;
        }
        if let Some(ref base_url) = llm.base_url {
            let new_val = if base_url.is_empty() {
                None
            } else {
                Some(base_url.clone())
            };
            if new_val != config.llm.base_url {
                config.llm.base_url = new_val;
                llm_changed = true;
            }
        }
        if let Some(v) = llm.gemini_google_search {
            if v != config.llm.gemini_google_search {
                config.llm.gemini_google_search = v;
                llm_changed = true;
            }
        }
    }

    // --- Server changes ---
    if let Some(ref server) = body.server {
        if let Some(ref system_prompt) = server.system_prompt {
            if *system_prompt != config.server.system_prompt {
                config.server.system_prompt = system_prompt.clone();
                system_prompt_changed = true;
            }
        }
        if let Some(ref display_name) = server.display_name {
            let new_val = if display_name.is_empty() {
                None
            } else {
                Some(display_name.clone())
            };
            config.server.display_name = new_val;
        }
    }

    // --- Weather changes ---
    let mut weather_key_changed = false;
    if let Some(ref weather) = body.weather {
        if let Some(ref key) = weather.pirate_weather_api_key {
            let new_val = if key.is_empty() { None } else { Some(key.clone()) };
            if new_val != config.weather.pirate_weather_api_key {
                config.weather.pirate_weather_api_key = new_val;
                weather_key_changed = true;
            }
        }
    }

    // --- Validate: try building a new LLM agent before committing ---
    if llm_changed || system_prompt_changed {
        // Build the agent (sync) and convert the error to String immediately
        // so that Box<dyn Error> (which isn't Send) doesn't live across .await.
        let agent_result = LlmAgent::from_config(
            &config.llm,
            &config.server.system_prompt,
            state.http_client.clone(),
        )
        .map_err(|e| e.to_string());

        match agent_result {
            Ok(new_agent) => {
                // Swap the agent
                let mut agent_guard = state.shared_agent.write().await;
                *agent_guard = Arc::new(new_agent);
                info!("hot-reloaded LLM agent (provider={}, model={})", config.llm.provider, config.llm.model);
            }
            Err(e) => {
                // Rollback config changes: re-read from the file since we already mutated in-place.
                warn!(error = %e, "failed to build LLM agent with new settings, rolling back");
                if let Ok(contents) = std::fs::read_to_string(&state.config_path) {
                    if let Ok(restored) = toml::from_str::<Config>(&contents) {
                        *config = restored;
                    }
                }
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid LLM configuration: {e}"),
                )
                    .into_response();
            }
        }
    }

    // --- Hot-reload weather key ---
    if weather_key_changed {
        let resolved = config.weather.resolve_api_key();
        let mut key_guard = state.shared_weather_key.write().await;
        *key_guard = resolved;
        info!("hot-reloaded weather API key");
    }

    // --- Persist to disk via toml_edit (format-preserving) ---
    if let Err(e) = persist_config(&state.config_path, &config) {
        warn!(error = %e, "failed to persist config to disk (in-memory changes are still active)");
        // Don't fail the request — in-memory state is already updated.
        // The user can retry or manually fix the file.
    }

    // Build response from the updated config.
    let settings = SettingsResponse {
        llm: LlmSettingsResponse {
            provider: config.llm.provider.clone(),
            model: config.llm.model.clone(),
            has_api_key: config.llm.resolve_api_key().is_some(),
            base_url: config.llm.base_url.clone(),
            gemini_google_search: config.llm.gemini_google_search,
        },
        server: ServerSettingsResponse {
            http_bind_addr: config.server.http_bind_addr.clone(),
            grpc_bind_addr: config.server.grpc_bind_addr.clone(),
            public_addr: config.server.public_addr.clone(),
            system_prompt: config.server.system_prompt.clone(),
            display_name: config.server.display_name.clone(),
        },
        storage: StorageSettingsResponse {
            media_dir: config.storage.media_dir.clone(),
            db_path: config.storage.db_path.clone(),
        },
        weather: WeatherSettingsResponse {
            has_api_key: config.weather.resolve_api_key().is_some(),
        },
    };

    info!("settings updated successfully");
    Json(settings).into_response()
}

/// Persist the config to disk using `toml_edit` for format-preserving writes.
/// Creates a `.bak` backup before overwriting.
fn persist_config(
    config_path: &std::path::Path,
    config: &Config,
) -> Result<(), String> {
    persist_config_inner(config_path, config).map_err(|e| e.to_string())
}

fn persist_config_inner(
    config_path: &std::path::Path,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use toml_edit::DocumentMut;

    // Read the existing file (or start from empty if it doesn't exist)
    let existing = if config_path.exists() {
        std::fs::read_to_string(config_path)?
    } else {
        String::new()
    };

    let mut doc: DocumentMut = existing.parse()?;

    // Helper: ensure a table exists in the document
    fn ensure_table<'a>(doc: &'a mut DocumentMut, key: &str) -> &'a mut toml_edit::Item {
        if doc.get(key).is_none() {
            doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        &mut doc[key]
    }

    // --- [llm] ---
    {
        let table = ensure_table(&mut doc, "llm");
        table["provider"] = toml_edit::value(&config.llm.provider);
        table["model"] = toml_edit::value(&config.llm.model);
        match &config.llm.api_key {
            Some(key) => table["api_key"] = toml_edit::value(key),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("api_key");
                }
            }
        }
        match &config.llm.base_url {
            Some(url) => table["base_url"] = toml_edit::value(url),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("base_url");
                }
            }
        }
        table["gemini_google_search"] = toml_edit::value(config.llm.gemini_google_search);
    }

    // --- [server] ---
    {
        let table = ensure_table(&mut doc, "server");
        if let Some(t) = table.as_table_mut() {
            t.remove("port");
        }
        table["http_bind_addr"] = toml_edit::value(&config.server.http_bind_addr);
        table["grpc_bind_addr"] = toml_edit::value(&config.server.grpc_bind_addr);
        table["public_addr"] = toml_edit::value(&config.server.public_addr);
        table["system_prompt"] = toml_edit::value(&config.server.system_prompt);
        match &config.server.display_name {
            Some(name) => table["display_name"] = toml_edit::value(name),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("display_name");
                }
            }
        }
    }

    // --- [storage] --- (read-only, but write it to keep the file complete)
    {
        let table = ensure_table(&mut doc, "storage");
        table["media_dir"] = toml_edit::value(&config.storage.media_dir);
        table["db_path"] = toml_edit::value(&config.storage.db_path);
    }

    // --- [weather] ---
    {
        let table = ensure_table(&mut doc, "weather");
        match &config.weather.pirate_weather_api_key {
            Some(key) => table["pirate_weather_api_key"] = toml_edit::value(key),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("pirate_weather_api_key");
                }
            }
        }
    }

    // Create .bak before writing
    if config_path.exists() {
        let bak = config_path.with_extension("toml.bak");
        std::fs::copy(config_path, &bak)?;
    }

    std::fs::write(config_path, doc.to_string())?;
    info!(path = %config_path.display(), "config persisted to disk");

    Ok(())
}

// ─── Event stream (streaming fetch / NDJSON) ────────────────────────

async fn event_stream(State(state): State<ApiState>) -> Response {
    let mut rx = state.events_tx.subscribe();
    build_ndjson_stream_response(
        async_stream::stream! {
            // Immediately send a heartbeat so the client knows the connection is live.
            yield Ok::<_, std::convert::Infallible>(
                format!("{}\n", serde_json::to_string(&Event::Heartbeat).unwrap())
            );

            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
            heartbeat.tick().await; // consume the immediate first tick

            loop {
                tokio::select! {
                    result = rx.recv() => {
                        match result {
                            Ok(event) => {
                                let line = format!("{}\n", serde_json::to_string(&event).unwrap());
                                yield Ok(line);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(missed = n, "event stream client lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                    _ = heartbeat.tick() => {
                        yield Ok(
                            format!("{}\n", serde_json::to_string(&Event::Heartbeat).unwrap())
                        );
                    }
                }
            }
        },
    )
}

async fn esim_event_stream(State(state): State<ApiState>) -> Response {
    let mut rx = state.esim_bridge.subscribe();
    build_ndjson_stream_response(
        async_stream::stream! {
            yield Ok::<_, std::convert::Infallible>(
                format!("{}\n", serde_json::json!({"type":"esim.heartbeat"}))
            );

            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
            heartbeat.tick().await;

            loop {
                tokio::select! {
                    result = rx.recv() => {
                        match result {
                            Ok(event) => {
                                let line = format!("{}\n", serde_json::to_string(&event).unwrap());
                                yield Ok(line);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(missed = n, "eSIM event stream client lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                    _ = heartbeat.tick() => {
                        yield Ok(
                            format!("{}\n", serde_json::json!({"type":"esim.heartbeat"}))
                        );
                    }
                }
            }
        },
    )
}

fn build_ndjson_stream_response<S>(stream: S) -> Response
where
    S: futures::stream::Stream<Item = Result<String, std::convert::Infallible>> + Send + 'static,
{
    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
}

// ─── Logs ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogQuery {
    /// Optional cap on returned lines (tail of the log). 0 / unset = all.
    #[serde(default)]
    pub lines: Option<usize>,
    /// If true (default), concatenate all rolled files in chronological order.
    /// If false, only return the most recent file.
    #[serde(default)]
    pub all: Option<bool>,
}

/// GET /api/logs/server — returns the on-disk rolling log files as text/plain.
///
/// By default, all rolled files in `logging.log_dir` are concatenated in
/// chronological order. Use `?lines=N` to return only the last N lines, or
/// `?all=false` to read just the most recent file.
async fn get_server_logs(
    State(state): State<ApiState>,
    axum::extract::Query(query): axum::extract::Query<LogQuery>,
) -> Response {
    let Some(log_dir) = state.log_dir.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "file logging is not configured (set logging.log_dir in config.toml)",
        )
            .into_response();
    };

    let prefix = state.log_file_prefix.as_str();

    // Discover candidate files: `{prefix}*` in `log_dir`, sorted by name
    // (rolling-file daily filenames are `{prefix}.YYYY-MM-DD`, lexicographically
    // sorted == chronologically sorted).
    let mut files: Vec<PathBuf> = match std::fs::read_dir(log_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(prefix))
                        .unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            tracing::error!(dir = %log_dir.display(), error = %e, "failed to read log dir");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read log dir: {e}"),
            )
                .into_response();
        }
    };
    files.sort();

    if files.is_empty() {
        return (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            String::new(),
        )
            .into_response();
    }

    let want_all = query.all.unwrap_or(true);
    let selected: &[PathBuf] = if want_all {
        &files
    } else {
        &files[files.len() - 1..]
    };

    // Read everything. Logs are typically small enough; large deployments
    // should use ?lines=. We accept the memory cost for simplicity.
    let mut buf = Vec::new();
    for path in selected {
        match tokio::fs::read(path).await {
            Ok(mut bytes) => buf.append(&mut bytes),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read log file");
            }
        }
    }

    let body = match query.lines {
        Some(n) if n > 0 => tail_lines(&buf, n),
        _ => buf,
    };

    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Return the last `n` lines from `bytes`. Operates on raw bytes to avoid
/// requiring valid UTF-8 (logs may include arbitrary bytes).
fn tail_lines(bytes: &[u8], n: usize) -> Vec<u8> {
    if n == 0 || bytes.is_empty() {
        return bytes.to_vec();
    }
    let mut count = 0usize;
    // Walk from the end, counting newlines.
    let mut idx = bytes.len();
    while idx > 0 {
        idx -= 1;
        if bytes[idx] == b'\n' {
            count += 1;
            if count > n {
                return bytes[idx + 1..].to_vec();
            }
        }
    }
    bytes.to_vec()
}

/// GET /api/logs/logcat — returns the device's full logcat buffer as text/plain.
///
/// Only available on Android. Returns 503 on other platforms. Uses
/// `logcat -d` (dump-and-exit). Use `?lines=N` to tail the output.
#[cfg(target_os = "android")]
async fn get_logcat_logs(
    axum::extract::Query(query): axum::extract::Query<LogQuery>,
) -> Response {
    use tokio::process::Command;

    let output = match Command::new("logcat").args(["-d"]).output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn logcat");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to run logcat: {e}"),
            )
                .into_response();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(status = ?output.status, %stderr, "logcat exited non-zero");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("logcat failed: {stderr}"),
        )
            .into_response();
    }

    let body = match query.lines {
        Some(n) if n > 0 => tail_lines(&output.stdout, n),
        _ => output.stdout,
    };

    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

#[cfg(not(target_os = "android"))]
async fn get_logcat_logs(
    axum::extract::Query(_query): axum::extract::Query<LogQuery>,
) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "logcat is only available on Android",
    )
        .into_response()
}
