use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

const BRIDGE_ADDR: &str = "127.0.0.1:16790";
const REQUEST_TTL_MS: u64 = 60 * 60 * 1000;
const MAX_STORED_REQUESTS: usize = 256;
const MAX_EVENTS_PER_REQUEST: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct EsimRequestRecord {
    pub request_id: String,
    pub action: String,
    pub status: String,
    pub accepted: bool,
    pub events: Vec<Value>,
    pub final_event: Option<Value>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EsimSnapshot {
    pub connected: bool,
    pub requests: Vec<EsimRequestRecord>,
}

#[derive(Debug, Clone)]
pub enum EsimRequestError {
    BridgeError {
        event: Value,
    },
    Timeout {
        request_id: String,
    },
    Internal {
        request_id: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub enum CellularStatusError {
    BridgeError(Value),
    Timeout { request_id: String },
    Internal(String),
}

#[derive(Debug, Clone)]
pub enum DeviceToggleError {
    BridgeError(Value),
    Timeout { request_id: String },
    Internal(String),
}

#[derive(Clone)]
pub struct EsimBridge {
    state: Arc<BridgeState>,
}

struct BridgeState {
    connected: RwLock<bool>,
    requests: Mutex<HashMap<String, EsimRequestRecord>>,
    acceptance_waiters: Mutex<HashMap<String, oneshot::Receiver<Result<(), String>>>>,
    command_tx: mpsc::UnboundedSender<OutboundMessage>,
    events_tx: broadcast::Sender<Value>,
}

struct OutboundMessage {
    body: Value,
    accepted_tx: Option<oneshot::Sender<Result<(), String>>>,
}

#[derive(Debug, Deserialize)]
struct BridgeEnvelope {
    #[serde(rename = "type")]
    message_type: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
}

impl EsimBridge {
    pub fn start() -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (events_tx, _) = broadcast::channel(256);
        let state = Arc::new(BridgeState {
            connected: RwLock::new(false),
            requests: Mutex::new(HashMap::new()),
            acceptance_waiters: Mutex::new(HashMap::new()),
            command_tx,
            events_tx,
        });

        tokio::spawn(run_bridge(state.clone(), command_rx));

        Self { state }
    }

    pub async fn submit_request(&self, action: String, payload: Value) -> Result<String, String> {
        let request_id = self.enqueue_request(action, payload).await?;
        self.wait_for_acceptance(&request_id).await?;
        Ok(request_id)
    }

    pub async fn submit_request_and_wait(
        &self,
        action: String,
        payload: Value,
        terminal_types: &[&str],
        timeout: std::time::Duration,
    ) -> Result<Value, EsimRequestError> {
        let request_id = self
            .enqueue_request(action, payload)
            .await
            .map_err(|message| EsimRequestError::Internal {
                request_id: None,
                message,
            })?;
        self.wait_for_acceptance(&request_id)
            .await
            .map_err(|message| EsimRequestError::Internal {
                request_id: Some(request_id.clone()),
                message,
            })?;
        self.wait_for_terminal_event(&request_id, terminal_types, timeout)
            .await
    }

    async fn enqueue_request(&self, action: String, payload: Value) -> Result<String, String> {
        let request_id = format!("req_{}", Uuid::new_v4().simple());
        let now_ms = now_ms();
        let record = EsimRequestRecord {
            request_id: request_id.clone(),
            action: action.clone(),
            status: "pending".into(),
            accepted: false,
            events: Vec::new(),
            final_event: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        self.state
            .requests
            .lock()
            .await
            .insert(request_id.clone(), record);

        let body = serde_json::json!({
            "type": "esim.request",
            "request_id": request_id,
            "action": action,
            "payload": payload,
        });

        let (accepted_tx, accepted_rx) = oneshot::channel();
        self.state
            .command_tx
            .send(OutboundMessage {
                body,
                accepted_tx: Some(accepted_tx),
            })
            .map_err(|_| "bridge command queue closed".to_string())?;

        self.state
            .requests
            .lock()
            .await
            .get_mut(&request_id)
            .ok_or_else(|| "request record missing after enqueue".to_string())?
            .status = "waiting_accept".into();

        self.state
            .acceptance_waiters
            .lock()
            .await
            .insert(request_id.clone(), accepted_rx);
        Ok(request_id)
    }

    async fn wait_for_acceptance(&self, request_id: &str) -> Result<(), String> {
        let accepted_rx = self
            .state
            .acceptance_waiters
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| "missing acceptance waiter".to_string())?;

        accepted_rx
            .await
            .map_err(|_| "bridge acceptance channel closed".to_string())??;
        Ok(())
    }

    async fn wait_for_terminal_event(
        &self,
        request_id: &str,
        terminal_types: &[&str],
        timeout: std::time::Duration,
    ) -> Result<Value, EsimRequestError> {
        let mut rx = self.state.events_tx.subscribe();

        if let Some(existing) = self
            .state
            .requests
            .lock()
            .await
            .get(request_id)
            .and_then(|record| record.final_event.clone())
        {
            if bridge_error_for_request(&existing, request_id)
                || explicit_error_event_for_request(&existing, request_id)
            {
                return Err(EsimRequestError::BridgeError { event: existing });
            }
            if matches_terminal_type(&existing, terminal_types) {
                return Ok(existing);
            }
        }

        let request_id = request_id.to_string();
        let timeout_request_id = request_id.clone();
        let terminal_types = terminal_types
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>();

        let wait = async move {
            loop {
                let event = rx.recv().await.map_err(|e| EsimRequestError::Internal {
                    request_id: Some(request_id.clone()),
                    message: e.to_string(),
                })?;
                let event_request_id = event.get("request_id").and_then(Value::as_str);
                if event_request_id != Some(request_id.as_str()) {
                    continue;
                }

                if bridge_error_for_request(&event, &request_id)
                    || explicit_error_event_for_request(&event, &request_id)
                {
                    return Err(EsimRequestError::BridgeError { event });
                }

                let event_type = event.get("type").and_then(Value::as_str);
                if event_type
                    .map(|value| terminal_types.iter().any(|item| item == value))
                    .unwrap_or(false)
                {
                    return Ok(event);
                }
            }
        };

        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| EsimRequestError::Timeout {
                request_id: timeout_request_id,
            })?
    }

    pub async fn snapshot(&self) -> EsimSnapshot {
        let connected = *self.state.connected.read().await;
        let mut requests = self
            .state
            .requests
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        requests.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));

        EsimSnapshot {
            connected,
            requests,
        }
    }

    pub async fn get_request(&self, request_id: &str) -> Option<EsimRequestRecord> {
        self.state.requests.lock().await.get(request_id).cloned()
    }

    pub async fn get_cellular_status(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Value, CellularStatusError> {
        let request_id = format!("cellular_{}", Uuid::new_v4().simple());
        let body = serde_json::json!({
            "type": "cellular.status_request",
            "request_id": request_id,
        });
        self.state
            .command_tx
            .send(OutboundMessage {
                body,
                accepted_tx: None,
            })
            .map_err(|_| {
                CellularStatusError::Internal("bridge command queue closed".to_string())
            })?;

        let mut rx = self.state.events_tx.subscribe();
        let wait_request_id = request_id.clone();
        let wait = async move {
            loop {
                let event = rx.recv().await.map_err(|error| match error {
                    RecvError::Lagged(missed) => CellularStatusError::Internal(format!(
                        "cellular status stream lagged by {missed}"
                    )),
                    RecvError::Closed => {
                        CellularStatusError::Internal("cellular status stream closed".to_string())
                    }
                })?;
                let event_request_id = event.get("request_id").and_then(Value::as_str);
                if event_request_id != Some(wait_request_id.as_str()) {
                    continue;
                }
                match event.get("type").and_then(Value::as_str) {
                    Some("cellular.status_result") => return Ok(event),
                    Some("cellular.status_error") => {
                        return Err(CellularStatusError::BridgeError(event))
                    }
                    _ => continue,
                }
            }
        };

        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| CellularStatusError::Timeout { request_id })?
    }

    pub async fn set_wifi_enabled(
        &self,
        enabled: bool,
        timeout: std::time::Duration,
    ) -> Result<Value, DeviceToggleError> {
        self.send_simple_request_and_wait(
            "wifi.set_enabled_request",
            serde_json::json!({ "enabled": enabled }),
            "wifi.set_enabled_result",
            "wifi.set_enabled_error",
            timeout,
        )
        .await
    }

    pub async fn set_cellular_enabled(
        &self,
        enabled: bool,
        timeout: std::time::Duration,
    ) -> Result<Value, DeviceToggleError> {
        self.send_simple_request_and_wait(
            "cellular.set_enabled_request",
            serde_json::json!({ "enabled": enabled }),
            "cellular.set_enabled_result",
            "cellular.set_enabled_error",
            timeout,
        )
        .await
    }

    async fn send_simple_request_and_wait(
        &self,
        message_type: &str,
        payload: Value,
        success_type: &str,
        error_type: &str,
        timeout: std::time::Duration,
    ) -> Result<Value, DeviceToggleError> {
        let request_id = format!("toggle_{}", Uuid::new_v4().simple());
        let body = serde_json::json!({
            "type": message_type,
            "request_id": request_id,
            "payload": payload,
        });
        self.state
            .command_tx
            .send(OutboundMessage {
                body,
                accepted_tx: None,
            })
            .map_err(|_| DeviceToggleError::Internal("bridge command queue closed".to_string()))?;

        let mut rx = self.state.events_tx.subscribe();
        let wait_request_id = request_id.clone();
        let success_type = success_type.to_string();
        let error_type = error_type.to_string();
        let wait = async move {
            loop {
                let event = rx.recv().await.map_err(|error| match error {
                    RecvError::Lagged(missed) => DeviceToggleError::Internal(format!(
                        "device toggle stream lagged by {missed}"
                    )),
                    RecvError::Closed => {
                        DeviceToggleError::Internal("device toggle stream closed".to_string())
                    }
                })?;
                let event_request_id = event.get("request_id").and_then(Value::as_str);
                if event_request_id != Some(wait_request_id.as_str()) {
                    continue;
                }
                match event.get("type").and_then(Value::as_str) {
                    Some(value) if value == success_type => return Ok(event),
                    Some(value) if value == error_type => {
                        return Err(DeviceToggleError::BridgeError(event))
                    }
                    _ => continue,
                }
            }
        };

        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| DeviceToggleError::Timeout { request_id })?
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.state.events_tx.subscribe()
    }
}

async fn run_bridge(
    state: Arc<BridgeState>,
    mut command_rx: mpsc::UnboundedReceiver<OutboundMessage>,
) {
    loop {
        match TcpStream::connect(BRIDGE_ADDR).await {
            Ok(stream) => {
                info!(addr = BRIDGE_ADDR, "connected to eSIM bridge");
                *state.connected.write().await = true;
                if let Err(error) = handle_connection(state.clone(), stream, &mut command_rx).await
                {
                    warn!(%error, "eSIM bridge connection ended");
                }
                *state.connected.write().await = false;
            }
            Err(error) => {
                warn!(%error, addr = BRIDGE_ADDR, "failed to connect to eSIM bridge");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

async fn handle_connection(
    state: Arc<BridgeState>,
    stream: TcpStream,
    command_rx: &mut mpsc::UnboundedReceiver<OutboundMessage>,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut pending_accept = HashMap::<String, oneshot::Sender<Result<(), String>>>::new();

    loop {
        tokio::select! {
            maybe_command = command_rx.recv() => {
                let Some(command) = maybe_command else {
                    return Err("bridge command queue closed".into());
                };
                if let Some(request_id) = command.body.get("request_id").and_then(Value::as_str) {
                    if let Some(accepted_tx) = command.accepted_tx {
                        pending_accept.insert(request_id.to_string(), accepted_tx);
                    }
                }
                let encoded = serde_json::to_string(&command.body).map_err(|e| e.to_string())?;
                writer.write_all(encoded.as_bytes()).await.map_err(|e| e.to_string())?;
                writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
                writer.flush().await.map_err(|e| e.to_string())?;
            }
            maybe_line = lines.next_line() => {
                let line = maybe_line.map_err(|e| e.to_string())?;
                let Some(line) = line else {
                    return Err("bridge socket closed".into());
                };
                if line.is_blank() {
                    continue;
                }
                let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                let envelope: BridgeEnvelope = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                handle_incoming_message(&state, envelope, value, &mut pending_accept).await;
            }
        }
    }
}

fn payload_result_is_error(result: &str) -> bool {
    matches!(result, "error" | "protected" | "disallowed_profile")
}

fn explicit_error_event_for_request(event: &Value, request_id: &str) -> bool {
    if event.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return false;
    }

    event
        .get("payload")
        .and_then(|payload| payload.get("result"))
        .and_then(Value::as_str)
        .map(payload_result_is_error)
        .unwrap_or(false)
}

fn download_verify_enable_event_is_final(envelope: &BridgeEnvelope) -> bool {
    match envelope.message_type.as_str() {
        "esim.download_result" => envelope
            .payload
            .as_ref()
            .and_then(|payload| payload.get("result"))
            .and_then(Value::as_str)
            .map(payload_result_is_error)
            .unwrap_or(false),
        "esim.profile_mutation_result" => {
            let payload = match envelope.payload.as_ref() {
                Some(payload) => payload,
                None => return false,
            };
            let operation = payload
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("");
            let result = payload.get("result").and_then(Value::as_str).unwrap_or("");
            operation == "enable" || (operation == "unknown" && result == "error")
        }
        _ => false,
    }
}

fn is_final_event_for_action(action: &str, envelope: &BridgeEnvelope) -> bool {
    if action == "humane.connectivity.esimlpa.downloadVerifyAndEnableProfile" {
        return download_verify_enable_event_is_final(envelope);
    }

    matches!(
        envelope.message_type.as_str(),
        "esim.profiles_result"
            | "esim.active_profile_result"
            | "esim.active_iccid_result"
            | "esim.device_identifiers_result"
            | "esim.profile_mutation_result"
            | "esim.download_result"
    )
}

async fn handle_incoming_message(
    state: &Arc<BridgeState>,
    envelope: BridgeEnvelope,
    value: Value,
    pending_accept: &mut HashMap<String, oneshot::Sender<Result<(), String>>>,
) {
    if let Some(request_id) = envelope.request_id.as_deref() {
        let mut requests = state.requests.lock().await;
        prune_requests(&mut requests);
        if let Some(record) = requests.get_mut(request_id) {
            let now_ms = now_ms();
            match envelope.message_type.as_str() {
                "esim.request_accepted" => {
                    record.accepted = true;
                    record.status = "accepted".into();
                    record.updated_at_ms = now_ms;
                    if let Some(tx) = pending_accept.remove(request_id) {
                        let _ = tx.send(Ok(()));
                    }
                }
                "esim.bridge_error" => {
                    record.status = "error".into();
                    record.final_event = Some(value.clone());
                    push_request_event(record, value.clone());
                    record.updated_at_ms = now_ms;
                    if let Some(tx) = pending_accept.remove(request_id) {
                        let message = envelope
                            .payload
                            .as_ref()
                            .and_then(|payload| payload.get("message"))
                            .and_then(Value::as_str)
                            .or_else(|| value.get("message").and_then(Value::as_str))
                            .unwrap_or("bridge error")
                            .to_string();
                        let _ = tx.send(Err(message));
                    }
                }
                _ => {
                    push_request_event(record, value.clone());
                    record.updated_at_ms = now_ms;
                    if record.final_event.is_none()
                        && is_final_event_for_action(&record.action, &envelope)
                    {
                        record.final_event = Some(value.clone());
                        let result = envelope
                            .payload
                            .as_ref()
                            .and_then(|payload| payload.get("result"))
                            .and_then(Value::as_str)
                            .unwrap_or("success");
                        record.status = if payload_result_is_error(result) {
                            "error".into()
                        } else {
                            "completed".into()
                        };
                    } else if record.status == "pending" {
                        record.status = "running".into();
                    }
                }
            }
        } else if envelope.message_type == "esim.request_accepted" {
            if let Some(tx) = pending_accept.remove(request_id) {
                let _ = tx.send(Ok(()));
            }
        }
    }

    let _ = state.events_tx.send(value);
}

trait BlankCheck {
    fn is_blank(&self) -> bool;
}

impl BlankCheck for String {
    fn is_blank(&self) -> bool {
        self.trim().is_empty()
    }
}

fn matches_terminal_type(event: &Value, terminal_types: &[&str]) -> bool {
    event
        .get("type")
        .and_then(Value::as_str)
        .map(|event_type| terminal_types.contains(&event_type))
        .unwrap_or(false)
}

fn bridge_error_for_request(event: &Value, request_id: &str) -> bool {
    event.get("type").and_then(Value::as_str) == Some("esim.bridge_error")
        && event.get("request_id").and_then(Value::as_str) == Some(request_id)
}

fn push_request_event(record: &mut EsimRequestRecord, event: Value) {
    record.events.push(event);
    if record.events.len() > MAX_EVENTS_PER_REQUEST {
        let overflow = record.events.len() - MAX_EVENTS_PER_REQUEST;
        record.events.drain(0..overflow);
    }
}

fn prune_requests(requests: &mut HashMap<String, EsimRequestRecord>) {
    let cutoff_ms = now_ms().saturating_sub(REQUEST_TTL_MS);
    requests.retain(|_, record| record.updated_at_ms >= cutoff_ms);

    if requests.len() <= MAX_STORED_REQUESTS {
        return;
    }

    let mut ordered = requests
        .iter()
        .map(|(request_id, record)| (request_id.clone(), record.updated_at_ms))
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.1.cmp(&a.1));

    let keep = ordered
        .into_iter()
        .take(MAX_STORED_REQUESTS)
        .map(|(request_id, _)| request_id)
        .collect::<std::collections::HashSet<_>>();
    requests.retain(|request_id, _| keep.contains(request_id));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
