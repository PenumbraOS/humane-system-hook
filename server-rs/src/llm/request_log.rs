use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{SecondsFormat, Utc};
use rig::completion::message::{AssistantContent, Message, UserContent};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tracing::warn;

const LLM_REQUEST_LOG_PREFIX: &str = "llm-requests";
const LLM_REQUEST_LOG_EXTENSION: &str = "jsonl";
const LLM_REQUEST_LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct LlmRequestLogger {
    log_dir: Arc<PathBuf>,
}

impl LlmRequestLogger {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            log_dir: Arc::new(log_dir),
        }
    }

    pub async fn log_chat(
        &self,
        provider: &str,
        run_id: &str,
        messages: &[Message],
        current_user_message: &str,
        response: Option<&str>,
        error: Option<&str>,
        latency_ms: u128,
    ) {
        let mut log_messages = messages_to_log_messages(messages);
        log_messages.push(LogMessage {
            role: "user",
            content: current_user_message.to_string(),
        });

        let record = ChatLogRecord {
            timestamp: timestamp(),
            provider,
            kind: "chat",
            run_id,
            latency_ms,
            request: ChatLogRequest {
                messages: log_messages,
            },
            response: response.map(|content| LogResponse {
                content: content.to_string(),
            }),
            error: error.map(ToOwned::to_owned),
        };

        self.append_record(&record).await;
    }

    async fn append_record<T>(&self, record: &T)
    where
        T: Serialize + ?Sized,
    {
        if let Err(error) = self.cleanup_old_logs().await {
            warn!(error = %error, "failed to clean up old LLM request logs");
        }

        if let Err(error) = tokio::fs::create_dir_all(self.log_dir.as_ref()).await {
            warn!(dir = %self.log_dir.display(), error = %error, "failed to create LLM request log dir");
            return;
        }

        let path = self.current_log_path();
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(error) => {
                warn!(error = %error, "failed to serialize LLM request log record");
                return;
            }
        };

        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(line.as_bytes()).await {
                    warn!(path = %path.display(), error = %error, "failed to write LLM request log record");
                    return;
                }
                if let Err(error) = file.write_all(b"\n").await {
                    warn!(path = %path.display(), error = %error, "failed to finish LLM request log record");
                }
            }
            Err(error) => {
                warn!(path = %path.display(), error = %error, "failed to open LLM request log file");
            }
        }
    }

    async fn cleanup_old_logs(&self) -> std::io::Result<()> {
        let now = SystemTime::now();
        let mut entries = match tokio::fs::read_dir(self.log_dir.as_ref()).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !is_llm_request_log_file(&path) {
                continue;
            }

            let metadata = entry.metadata().await?;
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };

            if age > LLM_REQUEST_LOG_RETENTION {
                tokio::fs::remove_file(&path).await?;
            }
        }

        Ok(())
    }

    fn current_log_path(&self) -> PathBuf {
        let date = Utc::now().format("%Y-%m-%d");
        self.log_dir.join(format!(
            "{LLM_REQUEST_LOG_PREFIX}.{date}.{LLM_REQUEST_LOG_EXTENSION}"
        ))
    }
}

#[derive(Serialize)]
struct ChatLogRecord<'a> {
    timestamp: String,
    provider: &'a str,
    kind: &'static str,
    run_id: &'a str,
    latency_ms: u128,
    request: ChatLogRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<LogResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct ChatLogRequest {
    messages: Vec<LogMessage>,
}

#[derive(Serialize)]
struct LogMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct LogResponse {
    content: String,
}

fn messages_to_log_messages(messages: &[Message]) -> Vec<LogMessage> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::System { content } => Some(LogMessage {
                role: "system",
                content: content.clone(),
            }),
            Message::User { content } => {
                message_content_to_text(content.iter().filter_map(|part| match part {
                    UserContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                }))
                .map(|content| LogMessage {
                    role: "user",
                    content,
                })
            }
            Message::Assistant { content, .. } => {
                message_content_to_text(content.iter().filter_map(|part| match part {
                    AssistantContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                }))
                .map(|content| LogMessage {
                    role: "assistant",
                    content,
                })
            }
        })
        .collect()
}

fn message_content_to_text<'a>(parts: impl Iterator<Item = &'a str>) -> Option<String> {
    let content = parts.collect::<Vec<_>>().join(" ");
    (!content.is_empty()).then_some(content)
}

fn is_llm_request_log_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                name.starts_with(&format!("{LLM_REQUEST_LOG_PREFIX}."))
                    && name.ends_with(&format!(".{LLM_REQUEST_LOG_EXTENSION}"))
            })
            .unwrap_or(false)
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
