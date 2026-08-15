//! REST/JSON API for the web portal.
//!
//! These endpoints are consumed by the Pin Center web app over the Local
//! Network Access (LNA) API.  All responses include CORS headers so the
//! public HTTPS portal can reach this HTTP server on the LAN.

mod contacts;
mod dev;
pub mod device;
mod settings_schema;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
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

use crate::config::{Config, LlmProvider, ResolvedConfig, ServerConfig};
use crate::db::Database;
use crate::dedup::DedupHandle;
use crate::esim::{
    CellularStatusError, DeviceToggleError, EsimBridge, EsimRequestError, EsimRequestRecord,
    EsimSnapshot,
};
use crate::llm::memory::MemoryService;
use crate::llm::{validate_prompt_template, LlmAgent, LlmRequestLogger};
use crate::nearby::NearbyClient;
use crate::services::aibus::{AiBus, AiBusHanders};
use crate::storage::{ConversationDetail, ConversationSummary, MediaStore, MemoryRecord};
use device::{DeviceApi, DeviceVersionSnapshot};

const ESIM_GETTER_TIMEOUT: Duration = Duration::from_secs(20);
const CELLULAR_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const NETWORK_TOGGLE_TIMEOUT: Duration = Duration::from_secs(10);

// ─── Shared state ───────────────────────────────────────────────────

/// State shared across all API handlers.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<MediaStore>>,
    pub db: Database,
    pub events_tx: tokio::sync::broadcast::Sender<Event>,
    /// Path to config.toml on disk — needed for writing settings back.
    pub config_path: PathBuf,
    /// Live config that can be updated at runtime.
    pub shared_config: Arc<RwLock<Config>>,
    /// Hot-swappable AiBus service root.
    pub aibus: AiBus,
    /// Dedup cache handle for invalidating stale gRPC responses after config swaps.
    pub dedup: DedupHandle,
    /// Always-on LLM request/response logger.
    pub llm_request_logger: LlmRequestLogger,
    /// Shared HTTP client for outbound requests.
    pub http_client: HttpClient,
    /// Directory where rolling log files are written, if file logging is enabled.
    pub log_dir: Option<PathBuf>,
    /// File-name prefix for rolling log files.
    pub log_file_prefix: String,
    /// Persistent eSIM bridge to the Android server app.
    pub esim_bridge: EsimBridge,
    /// One-shot flag consumed by the device contacts sync hook.
    pub contact_client_reset_pending: Arc<AtomicBool>,
    /// Current device software and OS versions.
    pub device_versions: DeviceVersionSnapshot,
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

// ─── Conversation types ─────────────────────────────────────────────

#[derive(Serialize)]
struct PaginatedConversations {
    conversations: Vec<ConversationSummary>,
    has_more: bool,
}

#[derive(Deserialize)]
struct ConversationsQuery {
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

// ─── Router ─────────────────────────────────────────────────────────

/// Build the `/api/*` router.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/memories", get(list_memories))
        .route("/api/memories/{uuid}", get(get_memory))
        .route("/api/memories/{uuid}", delete(delete_memory))
        .route("/api/memories/{uuid}/thumbnail/{index}", get(get_thumbnail))
        .route("/api/memories/{uuid}/files/{filename}", get(get_file))
        .route("/api/conversations", get(list_conversations))
        .route("/api/conversations/{id}", get(get_conversation))
        .nest("/api", contacts::router())
        .route("/api/device", get(DeviceApi::get_device))
        .route("/api/settings", get(get_settings))
        .route("/api/settings", put(update_settings))
        .route("/api/events", get(event_stream))
        .route(
            "/api/cellular/service-status",
            get(get_cellular_service_status),
        )
        .route("/api/cellular/set-enabled", put(set_cellular_enabled))
        .route("/api/wifi/set-enabled", put(set_wifi_enabled))
        .route("/api/logs/server", get(get_server_logs))
        .route("/api/logs/logcat", get(get_logcat_logs))
        .nest("/api/dev", dev::router())
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
        .route(
            "/api/esim/download-verify-enable",
            put(esim_download_verify_enable),
        )
        .merge(settings_schema::router())
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
            let _ = state
                .events_tx
                .send(Event::MemoryDeleted { uuid: uuid.clone() });
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

// ─── Conversations ──────────────────────────────────────────────────

const CONVERSATIONS_DEFAULT_LIMIT: i64 = 50;
const CONVERSATIONS_MAX_LIMIT: i64 = 200;

async fn list_conversations(
    axum::extract::Query(query): axum::extract::Query<ConversationsQuery>,
    State(state): State<ApiState>,
) -> Json<PaginatedConversations> {
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query
        .limit
        .unwrap_or(CONVERSATIONS_DEFAULT_LIMIT)
        .max(1)
        .min(CONVERSATIONS_MAX_LIMIT);

    let conversations = match state.db.list_conversations(offset, limit).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to list conversations");
            Vec::new()
        }
    };

    Json(PaginatedConversations {
        has_more: conversations.len() == limit as usize,
        conversations,
    })
}

async fn get_conversation(
    Path(id): Path<i64>,
    State(state): State<ApiState>,
) -> Result<Json<ConversationDetail>, StatusCode> {
    match state.db.get_conversation(id).await {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            warn!(id, error = %e, "failed to get conversation");
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

// ─── Settings ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsResponse {
    llm: LlmSettingsResponse,
    server: ServerSettingsResponse,
    storage: StorageSettingsResponse,
    weather: WeatherSettingsResponse,
    contacts: ContactsSettingsResponse,
    dev: DevSettingsResponse,
}

#[derive(Serialize)]
struct LlmSettingsResponse {
    provider: LlmProvider,
    model: String,
    has_api_key: bool,
    base_url: Option<String>,
    web_search: bool,
    tools: LlmToolsSettingsResponse,
    memory: LlmMemorySettingsResponse,
}

#[derive(Serialize)]
struct LlmMemorySettingsResponse {
    enabled: bool,
    path: String,
    top_k: usize,
    snippet_chars: usize,
    max_context_chars: usize,
    auto_retrieve: bool,
    auto_remember: bool,
}

#[derive(Serialize)]
struct LlmToolsSettingsResponse {
    enabled: bool,
    dynamic_tool_count: usize,
    max_tool_turns: usize,
    tool_concurrency: usize,
}

#[derive(Serialize)]
struct ServerSettingsResponse {
    http_bind_addr: String,
    grpc_bind_addr: String,
    public_addr: String,
    system_prompt: String,
    status_prompt: String,
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

#[derive(Serialize)]
struct ContactsSettingsResponse {
    trust_all_contacts: bool,
    allow_all_inbound: bool,
}

#[derive(Serialize)]
struct DevSettingsResponse {
    apk_install_enabled: bool,
}

async fn get_settings(State(state): State<ApiState>) -> Json<SettingsResponse> {
    let config = state.shared_config.read().await;
    Json(SettingsResponse {
        llm: LlmSettingsResponse {
            provider: config.llm.provider,
            model: config.llm.model.clone(),
            has_api_key: config.llm.resolve_api_key().is_some(),
            base_url: config.llm.base_url.clone(),
            web_search: config.llm.web_search,
            tools: LlmToolsSettingsResponse {
                enabled: config.llm.tools.enabled,
                dynamic_tool_count: config.llm.tools.dynamic_tool_count,
                max_tool_turns: config.llm.tools.max_tool_turns,
                tool_concurrency: config.llm.tools.tool_concurrency,
            },
            memory: LlmMemorySettingsResponse {
                enabled: config.llm.memory.enabled,
                path: config.llm.memory.path.clone(),
                top_k: config.llm.memory.top_k,
                snippet_chars: config.llm.memory.snippet_chars,
                max_context_chars: config.llm.memory.max_context_chars,
                auto_retrieve: config.llm.memory.auto_retrieve,
                auto_remember: config.llm.memory.auto_remember,
            },
        },
        server: ServerSettingsResponse {
            http_bind_addr: config.server.http_bind_addr.clone(),
            grpc_bind_addr: config.server.grpc_bind_addr.clone(),
            public_addr: config.server.public_addr.clone(),
            system_prompt: config.server.resolved_system_prompt(),
            status_prompt: config.server.resolved_status_prompt(),
            display_name: config.server.display_name.clone(),
        },
        storage: StorageSettingsResponse {
            media_dir: config.storage.media_dir.clone(),
            db_path: config.storage.db_path.clone(),
        },
        weather: WeatherSettingsResponse {
            has_api_key: config.weather.resolve_api_key().is_some(),
        },
        contacts: ContactsSettingsResponse {
            trust_all_contacts: config.contacts.trust_all_contacts,
            allow_all_inbound: config.contacts.allow_all_inbound,
        },
        dev: DevSettingsResponse {
            apk_install_enabled: config.dev.apk_install_enabled,
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
    match state
        .esim_bridge
        .get_cellular_status(CELLULAR_STATUS_TIMEOUT)
        .await
    {
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
    match state
        .esim_bridge
        .set_cellular_enabled(body.enabled, NETWORK_TOGGLE_TIMEOUT)
        .await
    {
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
    match state
        .esim_bridge
        .set_wifi_enabled(body.enabled, NETWORK_TOGGLE_TIMEOUT)
        .await
    {
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
    )
    .await
}

async fn esim_disable_profile(
    State(state): State<ApiState>,
    Json(body): Json<EsimIccidRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.disableProfile",
        serde_json::json!({ "iccid": body.iccid }),
    )
    .await
}

async fn esim_set_nickname(
    State(state): State<ApiState>,
    Json(body): Json<EsimNicknameRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.setNickname",
        serde_json::json!({ "iccid": body.iccid, "nickname": body.nickname }),
    )
    .await
}

async fn esim_delete_profile(
    State(state): State<ApiState>,
    Json(body): Json<EsimIccidRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.deleteProfile",
        serde_json::json!({ "iccid": body.iccid }),
    )
    .await
}

async fn esim_download_verify_enable(
    State(state): State<ApiState>,
    Json(body): Json<EsimActivationCodeRequest>,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    submit_esim_request(
        &state,
        "humane.connectivity.esimlpa.downloadVerifyAndEnableProfile",
        serde_json::json!({ "activationCode": body.activation_code }),
    )
    .await
}

async fn submit_esim_request(
    state: &ApiState,
    action: &str,
    payload: serde_json::Value,
) -> Result<Json<EsimRequestAcceptedResponse>, StatusCode> {
    match state
        .esim_bridge
        .submit_request(action.to_string(), payload)
        .await
    {
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
        EsimRequestError::Internal {
            request_id,
            message,
        } => (
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
        CellularStatusError::BridgeError(event) => {
            (StatusCode::BAD_GATEWAY, Json(event)).into_response()
        }
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
        DeviceToggleError::BridgeError(event) => {
            (StatusCode::BAD_GATEWAY, Json(event)).into_response()
        }
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
    contacts: Option<UpdateContactsSettings>,
    dev: Option<UpdateDevSettings>,
    /// Storage is read-only; presence in the request is rejected.
    storage: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UpdateLlmSettings {
    provider: Option<LlmProvider>,
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    web_search: Option<bool>,
    tools: Option<UpdateLlmToolsSettings>,
    memory: Option<UpdateLlmMemorySettings>,
}

#[derive(Deserialize)]
struct UpdateLlmMemorySettings {
    enabled: Option<bool>,
    path: Option<String>,
    top_k: Option<usize>,
    snippet_chars: Option<usize>,
    max_context_chars: Option<usize>,
    auto_retrieve: Option<bool>,
    auto_remember: Option<bool>,
}

#[derive(Deserialize)]
struct UpdateLlmToolsSettings {
    enabled: Option<bool>,
    dynamic_tool_count: Option<usize>,
    max_tool_turns: Option<usize>,
    tool_concurrency: Option<usize>,
}

#[derive(Deserialize)]
struct UpdateServerSettings {
    /// Read-only — rejected if present.
    http_bind_addr: Option<serde_json::Value>,
    /// Read-only — rejected if present.
    grpc_bind_addr: Option<serde_json::Value>,
    /// Read-only — rejected if present.
    public_addr: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_prompt_update")]
    system_prompt: PromptUpdate,
    #[serde(default, deserialize_with = "deserialize_prompt_update")]
    status_prompt: PromptUpdate,
    display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptUpdate {
    Unchanged,
    Clear,
    Set(String),
}

impl Default for PromptUpdate {
    fn default() -> Self {
        Self::Unchanged
    }
}

fn deserialize_prompt_update<'de, D>(deserializer: D) -> Result<PromptUpdate, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        None => Ok(PromptUpdate::Clear),
        Some(prompt) if prompt.is_empty() => Ok(PromptUpdate::Clear),
        Some(prompt) if prompt.trim().is_empty() => {
            Err(serde::de::Error::custom("prompt cannot be whitespace-only"))
        }
        Some(prompt) => Ok(PromptUpdate::Set(prompt.trim().to_string())),
    }
}

#[derive(Deserialize)]
struct UpdateWeatherSettings {
    pirate_weather_api_key: Option<String>,
}

#[derive(Deserialize)]
struct UpdateContactsSettings {
    trust_all_contacts: Option<bool>,
    allow_all_inbound: Option<bool>,
}

#[derive(Deserialize)]
struct UpdateDevSettings {
    apk_install_enabled: Option<bool>,
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

    if let Some(ref server) = body.server {
        if let PromptUpdate::Set(system_prompt) = &server.system_prompt {
            if let Err(error) = validate_prompt_template("server.system_prompt", system_prompt) {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid server.system_prompt template: {error}"),
                )
                    .into_response();
            }
        }
        if let PromptUpdate::Set(status_prompt) = &server.status_prompt {
            if let Err(error) = validate_prompt_template("server.status_prompt", status_prompt) {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid server.status_prompt template: {error}"),
                )
                    .into_response();
            }
        }
    }

    // Take a write lock on the source config and apply changes.
    let mut config = state.shared_config.write().await;
    let original_config = config.clone();

    // --- LLM changes ---
    if let Some(ref llm) = body.llm {
        if let Some(provider) = llm.provider {
            if provider != config.llm.provider {
                config.llm.provider = provider;
            }
        }
        if let Some(ref model) = llm.model {
            if *model != config.llm.model {
                config.llm.model = model.clone();
            }
        }
        if let Some(ref api_key) = llm.api_key {
            config.llm.api_key = if api_key.is_empty() {
                None
            } else {
                Some(api_key.clone())
            };
        }
        if let Some(ref base_url) = llm.base_url {
            let new_val = if base_url.is_empty() {
                None
            } else {
                Some(base_url.clone())
            };
            if new_val != config.llm.base_url {
                config.llm.base_url = new_val;
            }
        }
        if let Some(v) = llm.web_search {
            if v != config.llm.web_search {
                config.llm.web_search = v;
            }
        }
        if let Some(ref tools) = llm.tools {
            if let Some(enabled) = tools.enabled {
                if enabled != config.llm.tools.enabled {
                    config.llm.tools.enabled = enabled;
                }
            }
            if let Some(dynamic_tool_count) = tools.dynamic_tool_count {
                if dynamic_tool_count != config.llm.tools.dynamic_tool_count {
                    config.llm.tools.dynamic_tool_count = dynamic_tool_count;
                }
            }
            if let Some(max_tool_turns) = tools.max_tool_turns {
                if max_tool_turns != config.llm.tools.max_tool_turns {
                    config.llm.tools.max_tool_turns = max_tool_turns;
                }
            }
            if let Some(tool_concurrency) = tools.tool_concurrency {
                if tool_concurrency != config.llm.tools.tool_concurrency {
                    config.llm.tools.tool_concurrency = tool_concurrency;
                }
            }
        }
        if let Some(ref memory) = llm.memory {
            if let Some(enabled) = memory.enabled {
                config.llm.memory.enabled = enabled;
            }
            if let Some(ref path) = memory.path {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    *config = original_config;
                    return (StatusCode::BAD_REQUEST, "llm.memory.path cannot be empty")
                        .into_response();
                }
                if trimmed != config.llm.memory.path {
                    config.llm.memory.path = trimmed.to_string();
                }
            }
            if let Some(top_k) = memory.top_k {
                config.llm.memory.top_k = top_k.max(1);
            }
            if let Some(snippet_chars) = memory.snippet_chars {
                config.llm.memory.snippet_chars = snippet_chars.max(1);
            }
            if let Some(max_context_chars) = memory.max_context_chars {
                config.llm.memory.max_context_chars = max_context_chars.max(1);
            }
            if let Some(auto_retrieve) = memory.auto_retrieve {
                config.llm.memory.auto_retrieve = auto_retrieve;
            }
            if let Some(auto_remember) = memory.auto_remember {
                config.llm.memory.auto_remember = auto_remember;
            }
        }
    }

    // --- Server changes ---
    if let Some(ref server) = body.server {
        match &server.system_prompt {
            PromptUpdate::Unchanged => {}
            PromptUpdate::Clear => {
                if config.server.system_prompt.is_some() {
                    config.server.system_prompt = None;
                }
            }
            PromptUpdate::Set(system_prompt) => {
                match ServerConfig::configured_system_prompt(system_prompt.clone()) {
                    Ok(new_val) => {
                        if new_val != config.server.system_prompt {
                            config.server.system_prompt = new_val;
                        }
                    }
                    Err(error) => {
                        *config = original_config;
                        return (StatusCode::BAD_REQUEST, error).into_response();
                    }
                }
            }
        }
        match &server.status_prompt {
            PromptUpdate::Unchanged => {}
            PromptUpdate::Clear => {
                if config.server.status_prompt.is_some() {
                    config.server.status_prompt = None;
                }
            }
            PromptUpdate::Set(status_prompt) => {
                match ServerConfig::configured_status_prompt(status_prompt.clone()) {
                    Ok(new_val) => {
                        if new_val != config.server.status_prompt {
                            config.server.status_prompt = new_val;
                        }
                    }
                    Err(error) => {
                        *config = original_config;
                        return (StatusCode::BAD_REQUEST, error).into_response();
                    }
                }
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
    if let Some(ref weather) = body.weather {
        if let Some(ref key) = weather.pirate_weather_api_key {
            let new_val = if key.is_empty() {
                None
            } else {
                Some(key.clone())
            };
            if new_val != config.weather.pirate_weather_api_key {
                config.weather.pirate_weather_api_key = new_val;
            }
        }
    }

    // --- Contacts changes ---
    if let Some(ref contacts) = body.contacts {
        if let Some(new_val) = contacts.trust_all_contacts {
            config.contacts.trust_all_contacts = new_val;
        }
        if let Some(new_val) = contacts.allow_all_inbound {
            config.contacts.allow_all_inbound = new_val;
        }
    }

    // --- Dev changes ---
    if let Some(ref dev) = body.dev {
        if let Some(new_val) = dev.apk_install_enabled {
            config.dev.apk_install_enabled = new_val;
        }
    }

    let new_resolved = Arc::new(ResolvedConfig::resolve(config.clone()));

    // --- Validate: build a new LLM/AiBus tree before committing ---
    let memory = if config.llm.memory.enabled {
        match MemoryService::open(config.llm.memory.clone()).await {
            Ok(memory) => Some(memory),
            Err(error) => {
                warn!(error = %error, "failed to initialize assistant memory, rolling back");
                *config = original_config;
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid memory configuration: {error}"),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    let agent_result = LlmAgent::from_config(
        &new_resolved,
        state.http_client.clone(),
        state.llm_request_logger.clone(),
        memory.clone(),
    )
    .await
    .map_err(|e| e.to_string());

    match agent_result {
        Ok(new_agent) => {
            let agent = Arc::new(new_agent);
            let new_aibus = Arc::new(AiBusHanders::new(
                agent,
                new_resolved.clone(),
                NearbyClient::new(state.http_client.clone()),
                state.http_client.clone(),
                state.db.clone(),
                memory,
            ));
            state.aibus.replace(new_aibus).await;
            state.dedup.clear().await;
            info!(
                provider = %config.llm.provider,
                model = %config.llm.model,
                "hot-reloaded AiBus service"
            );
        }
        Err(e) => {
            warn!(error = %e, "failed to build AiBus service with new settings, rolling back");
            *config = original_config;
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid LLM configuration: {e}"),
            )
                .into_response();
        }
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
            provider: config.llm.provider,
            model: config.llm.model.clone(),
            has_api_key: config.llm.resolve_api_key().is_some(),
            base_url: config.llm.base_url.clone(),
            web_search: config.llm.web_search,
            tools: LlmToolsSettingsResponse {
                enabled: config.llm.tools.enabled,
                dynamic_tool_count: config.llm.tools.dynamic_tool_count,
                max_tool_turns: config.llm.tools.max_tool_turns,
                tool_concurrency: config.llm.tools.tool_concurrency,
            },
            memory: LlmMemorySettingsResponse {
                enabled: config.llm.memory.enabled,
                path: config.llm.memory.path.clone(),
                top_k: config.llm.memory.top_k,
                snippet_chars: config.llm.memory.snippet_chars,
                max_context_chars: config.llm.memory.max_context_chars,
                auto_retrieve: config.llm.memory.auto_retrieve,
                auto_remember: config.llm.memory.auto_remember,
            },
        },
        server: ServerSettingsResponse {
            http_bind_addr: config.server.http_bind_addr.clone(),
            grpc_bind_addr: config.server.grpc_bind_addr.clone(),
            public_addr: config.server.public_addr.clone(),
            system_prompt: config.server.resolved_system_prompt(),
            status_prompt: config.server.resolved_status_prompt(),
            display_name: config.server.display_name.clone(),
        },
        storage: StorageSettingsResponse {
            media_dir: config.storage.media_dir.clone(),
            db_path: config.storage.db_path.clone(),
        },
        weather: WeatherSettingsResponse {
            has_api_key: config.weather.resolve_api_key().is_some(),
        },
        contacts: ContactsSettingsResponse {
            trust_all_contacts: config.contacts.trust_all_contacts,
            allow_all_inbound: config.contacts.allow_all_inbound,
        },
        dev: DevSettingsResponse {
            apk_install_enabled: config.dev.apk_install_enabled,
        },
    };

    info!("settings updated successfully");
    Json(settings).into_response()
}

/// Persist the config to disk using `toml_edit` for format-preserving writes.
/// Creates a `.bak` backup before overwriting.
fn persist_config(config_path: &std::path::Path, config: &Config) -> Result<(), String> {
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
        table["provider"] = toml_edit::value(config.llm.provider.as_str());
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
        table["web_search"] = toml_edit::value(config.llm.web_search);
    }

    // --- [llm.tools] ---
    {
        if doc["llm"].as_table_mut().is_none() {
            doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if doc["llm"]["tools"].as_table_mut().is_none() {
            doc["llm"]["tools"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        let table = &mut doc["llm"]["tools"];
        table["enabled"] = toml_edit::value(config.llm.tools.enabled);
        table["dynamic_tool_count"] = toml_edit::value(config.llm.tools.dynamic_tool_count as i64);
        table["max_tool_turns"] = toml_edit::value(config.llm.tools.max_tool_turns as i64);
        table["tool_concurrency"] = toml_edit::value(config.llm.tools.tool_concurrency as i64);

        if let Some(t) = table.as_table_mut() {
            t.remove("embedding_model");
        }
    }

    // --- [llm.memory] ---
    {
        if doc["llm"].as_table_mut().is_none() {
            doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if doc["llm"]["memory"].as_table_mut().is_none() {
            doc["llm"]["memory"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        let table = &mut doc["llm"]["memory"];
        table["enabled"] = toml_edit::value(config.llm.memory.enabled);
        table["path"] = toml_edit::value(&config.llm.memory.path);
        table["top_k"] = toml_edit::value(config.llm.memory.top_k as i64);
        table["snippet_chars"] = toml_edit::value(config.llm.memory.snippet_chars as i64);
        table["max_context_chars"] = toml_edit::value(config.llm.memory.max_context_chars as i64);
        table["auto_retrieve"] = toml_edit::value(config.llm.memory.auto_retrieve);
        table["auto_remember"] = toml_edit::value(config.llm.memory.auto_remember);
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
        match &config.server.system_prompt {
            Some(system_prompt) => table["system_prompt"] = toml_edit::value(system_prompt),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("system_prompt");
                }
            }
        }
        match &config.server.status_prompt {
            Some(status_prompt) => table["status_prompt"] = toml_edit::value(status_prompt),
            None => {
                if let Some(t) = table.as_table_mut() {
                    t.remove("status_prompt");
                }
            }
        }
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

    // --- [contacts] ---
    {
        let table = ensure_table(&mut doc, "contacts");
        table["trust_all_contacts"] = toml_edit::value(config.contacts.trust_all_contacts);
        table["allow_all_inbound"] = toml_edit::value(config.contacts.allow_all_inbound);
    }

    // --- [dev] ---
    {
        let table = ensure_table(&mut doc, "dev");
        table["apk_install_enabled"] = toml_edit::value(config.dev.apk_install_enabled);
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
    build_ndjson_stream_response(async_stream::stream! {
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
    })
}

async fn esim_event_stream(State(state): State<ApiState>) -> Response {
    let mut rx = state.esim_bridge.subscribe();
    build_ndjson_stream_response(async_stream::stream! {
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
    })
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

    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
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
async fn get_logcat_logs(axum::extract::Query(query): axum::extract::Query<LogQuery>) -> Response {
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

    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

#[cfg(not(target_os = "android"))]
async fn get_logcat_logs(axum::extract::Query(_query): axum::extract::Query<LogQuery>) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "logcat is only available on Android",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct PromptUpdateFixture {
        #[serde(default, deserialize_with = "deserialize_prompt_update")]
        prompt: PromptUpdate,
    }

    #[test]
    fn prompt_update_missing_is_unchanged() {
        let fixture: PromptUpdateFixture = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(fixture.prompt, PromptUpdate::Unchanged);
    }

    #[test]
    fn prompt_update_null_clears() {
        let fixture: PromptUpdateFixture = serde_json::from_str(r#"{"prompt":null}"#).unwrap();
        assert_eq!(fixture.prompt, PromptUpdate::Clear);
    }

    #[test]
    fn prompt_update_empty_string_clears() {
        let fixture: PromptUpdateFixture = serde_json::from_str(r#"{"prompt":""}"#).unwrap();
        assert_eq!(fixture.prompt, PromptUpdate::Clear);
    }

    #[test]
    fn prompt_update_string_sets_trimmed_custom_prompt() {
        let fixture: PromptUpdateFixture =
            serde_json::from_str(r#"{"prompt":"  Custom prompt  "}"#).unwrap();
        assert_eq!(fixture.prompt, PromptUpdate::Set("Custom prompt".into()));
    }

    #[test]
    fn prompt_update_whitespace_string_is_rejected() {
        let error = serde_json::from_str::<PromptUpdateFixture>(r#"{"prompt":"   "}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("prompt cannot be whitespace-only"));
    }
}
