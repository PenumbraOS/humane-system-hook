//! REST/JSON API for the web portal.
//!
//! These endpoints are consumed by the Pin Center web app over the Local
//! Network Access (LNA) API.  All responses include CORS headers so the
//! public HTTPS portal can reach this HTTP server on the LAN.

use std::path::PathBuf;
use std::sync::Arc;

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
use crate::llm::LlmAgent;
use crate::storage::{MediaStore, MemoryRecord};

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

    let stream = async_stream::stream! {
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
                            // Continue — the client will miss some events but stay connected.
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
    };

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
}
