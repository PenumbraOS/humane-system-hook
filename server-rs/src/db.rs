//! SQLite backed persistence for memory metadata and conversations.
//!
//! All methods use [`tokio::task::spawn_blocking`] internally so callers can
//! treat them as async without blocking the tokio runtime.

mod contacts;

use chrono::{Duration, Utc};
pub use contacts::{ContactImportError, ContactRecord};

use std::path::Path;
use std::sync::{Arc, Mutex};

use rig::completion::message::{AssistantContent, Message, UserContent};
use rusqlite::{params, Connection};
use tracing::{info, warn};

use crate::storage::{
    ConversationDetail, ConversationMessage, ConversationSummary, Location, MemoryRecord,
    MemoryStatus,
};

// ─── Schema ─────────────────────────────────────────────────────────

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS memories (
    uuid                    TEXT PRIMARY KEY,
    memory_type             TEXT NOT NULL,
    device_local_id         TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'pending',
    thumbnail_count         INTEGER NOT NULL DEFAULT 0,
    latitude                REAL,
    longitude               REAL,
    location_accuracy       REAL,
    location_human_readable TEXT,
    location_full_address   TEXT
);

CREATE TABLE IF NOT EXISTS memory_files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_uuid TEXT NOT NULL REFERENCES memories(uuid) ON DELETE CASCADE,
    filename    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_files_uuid ON memory_files(memory_uuid);

CREATE TABLE IF NOT EXISTS conversations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%s','now')),
    utterance  TEXT NOT NULL,
    is_vision  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS conversation_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    seq             INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conv_messages_conv ON conversation_messages(conversation_id);
CREATE INDEX IF NOT EXISTS idx_conversations_created ON conversations(created_at DESC);
"#;

/// Thread-safe handle to the SQLite database.
#[derive(Clone)]
pub struct Database {
    pub(super) conn: Arc<Mutex<Connection>>,
}

#[allow(dead_code)]
impl Database {
    /// Open (or create) the database at the given path and apply the schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(contacts::CONTACTS_SCHEMA)?;

        info!(path = %path.display(), "database opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a new memory record.
    pub async fn create_memory(
        &self,
        record: &MemoryRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let record = record.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;

            let (lat, lon, acc, human, full) = match &record.location {
                Some(loc) => (
                    Some(loc.latitude),
                    Some(loc.longitude),
                    loc.accuracy.map(|a| a as f64),
                    loc.human_readable.clone(),
                    loc.full_address.clone(),
                ),
                None => (None, None, None, None, None),
            };

            conn.execute(
                "INSERT INTO memories (uuid, memory_type, device_local_id, created_at, status,
                    thumbnail_count, latitude, longitude, location_accuracy,
                    location_human_readable, location_full_address)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    record.uuid,
                    record.memory_type,
                    record.device_local_id,
                    record.created_at,
                    status_to_str(&record.status),
                    record.thumbnail_count as i64,
                    lat,
                    lon,
                    acc,
                    human,
                    full,
                ],
            )?;

            // Insert associated files
            for filename in &record.files {
                conn.execute(
                    "INSERT INTO memory_files (memory_uuid, filename) VALUES (?1, ?2)",
                    params![record.uuid, filename],
                )?;
            }

            Ok(())
        })
        .await?
    }

    /// Update the status of a memory.
    pub async fn set_memory_status(
        &self,
        uuid: &str,
        status: &MemoryStatus,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let uuid = uuid.to_string();
        let status_str = status_to_str(status).to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let changed = conn.execute(
                "UPDATE memories SET status = ?1 WHERE uuid = ?2",
                params![status_str, uuid],
            )?;
            Ok(changed > 0)
        })
        .await?
    }

    /// Update the thumbnail count for a memory.
    pub async fn set_thumbnail_count(
        &self,
        uuid: &str,
        count: usize,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let uuid = uuid.to_string();
        let count = count as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            conn.execute(
                "UPDATE memories SET thumbnail_count = ?1 WHERE uuid = ?2",
                params![count, uuid],
            )?;
            Ok(())
        })
        .await?
    }

    /// Add a filename to a memory's file list (idempotent).
    pub async fn add_file(
        &self,
        uuid: &str,
        filename: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let uuid = uuid.to_string();
        let filename = filename.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            // Check if already exists
            let exists: bool = conn.query_row(
                "SELECT COUNT(*) > 0 FROM memory_files WHERE memory_uuid = ?1 AND filename = ?2",
                params![uuid, filename],
                |row| row.get(0),
            )?;
            if !exists {
                conn.execute(
                    "INSERT INTO memory_files (memory_uuid, filename) VALUES (?1, ?2)",
                    params![uuid, filename],
                )?;
            }
            Ok(())
        })
        .await?
    }

    /// Delete a memory record and its files from the database.
    pub async fn delete_memory(
        &self,
        uuid: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let uuid = uuid.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let changed = conn.execute("DELETE FROM memories WHERE uuid = ?1", params![uuid])?;
            Ok(changed > 0)
        })
        .await?
    }

    /// Look up a single memory by UUID.
    pub async fn get_memory(
        &self,
        uuid: &str,
    ) -> Result<Option<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let uuid = uuid.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            get_memory_inner(&conn, &uuid)
        })
        .await?
    }

    /// Find the memory that owns a given filename.
    pub async fn find_memory_for_file(
        &self,
        filename: &str,
    ) -> Result<Option<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let filename = filename.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let uuid: Option<String> = conn
                .query_row(
                    "SELECT memory_uuid FROM memory_files WHERE filename = ?1 LIMIT 1",
                    params![filename],
                    |row| row.get(0),
                )
                .ok();

            match uuid {
                Some(uuid) => get_memory_inner(&conn, &uuid),
                None => Ok(None),
            }
        })
        .await?
    }

    /// List all memories, ordered by creation time descending.
    pub async fn list_memories(
        &self,
    ) -> Result<Vec<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let mut stmt = conn.prepare(
                "SELECT uuid, memory_type, device_local_id, created_at, status,
                        thumbnail_count, latitude, longitude, location_accuracy,
                        location_human_readable, location_full_address
                 FROM memories ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| Ok(memory_from_row(row)))?;

            let mut memories = Vec::new();
            for row in rows {
                let mut record = row?;
                record.files = get_files_for_memory(&conn, &record.uuid)?;
                memories.push(record);
            }
            Ok(memories)
        })
        .await?
    }

    /// Persist a completed conversation.
    pub async fn save_conversation(
        &self,
        run_id: &str,
        utterance: &str,
        is_vision: bool,
        messages: &[(String, String)], // (role, content)
    ) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        let run_id = run_id.to_string();
        let utterance = utterance.to_string();
        let messages: Vec<(String, String)> = messages.to_vec();

        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let tx = conn.transaction()?;

            tx.execute(
                "INSERT INTO conversations (run_id, utterance, is_vision) VALUES (?1, ?2, ?3)",
                params![run_id, utterance, is_vision as i32],
            )?;
            let conversation_id = tx.last_insert_rowid();

            for (seq, (role, content)) in messages.iter().enumerate() {
                tx.execute(
                    "INSERT INTO conversation_messages (conversation_id, role, content, seq)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![conversation_id, role, content, seq as i64],
                )?;
            }

            tx.commit()?;

            Ok(conversation_id)
        })
        .await?
    }

    /// Persist an Understand conversation from rig [`Message`] history.
    ///
    /// Merges this converation with existing ones matching a predicate indicating the last message
    pub async fn save_understand_conversation(
        &self,
        run_id: &str,
        utterance: &str,
        is_vision: bool,
        history: &[Message],
        response_text: &str,
    ) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let mut pairs = messages_to_pairs(history);

        // Grab the last non-system message from history
        let prev_message =
            pairs
                .iter()
                .rev()
                .find(|(role, _)| role != "system")
                .map(|(role, content)| {
                    let seq = pairs
                        .iter()
                        .position(|(r, c)| r == role && c == content)
                        .unwrap_or(0) as i64;
                    (role.clone(), content.clone(), seq)
                });

        pairs.push(("user".into(), utterance.to_string()));
        pairs.push(("assistant".into(), response_text.to_string()));

        let conn = self.conn.clone();
        let run_id = run_id.to_string();
        let utterance = utterance.to_string();
        let messages: Vec<(String, String)> = pairs;

        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let tx = conn.transaction()?;

            // Look for a matching previous message in a recent conversation.
            let existing_id: Option<i64> =
                if let Some((prev_role, prev_content, prev_seq)) = &prev_message {
                    let cutoff = Utc::now()
                        .checked_sub_signed(Duration::minutes(10))
                        .unwrap_or_else(Utc::now)
                        .timestamp();

                    tx.query_row(
                        "SELECT c.id FROM conversations c
                        JOIN conversation_messages cm ON cm.conversation_id = c.id
                        WHERE cm.role = ?1 AND cm.content = ?2 AND cm.seq = ?3
                            AND CAST(c.created_at AS INTEGER) >= ?4
                        ORDER BY c.created_at DESC
                        LIMIT 1",
                        params![prev_role, prev_content, prev_seq, cutoff],
                        |row| row.get(0),
                    )
                    .ok()
                } else {
                    None
                };

            if let Some(id) = existing_id {
                // Merge into existing conversation.
                tx.execute(
                    "DELETE FROM conversation_messages WHERE conversation_id = ?1",
                    params![id],
                )?;

                for (seq, (role, content)) in messages.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO conversation_messages (conversation_id, role, content, seq)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, role, content, seq as i64],
                    )?;
                }

                tx.commit()?;

                Ok(id)
            } else {
                // No match, add new row
                tx.execute(
                    "INSERT INTO conversations (run_id, utterance, is_vision) VALUES (?1, ?2, ?3)",
                    params![run_id, utterance, is_vision as i32],
                )?;
                let conversation_id = tx.last_insert_rowid();

                for (seq, (role, content)) in messages.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO conversation_messages (conversation_id, role, content, seq)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![conversation_id, role, content, seq as i64],
                    )?;
                }

                tx.commit()?;

                Ok(conversation_id)
            }
        })
        .await?
    }

    /// List conversations with pagination
    pub async fn list_conversations(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<ConversationSummary>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;
            let mut stmt = conn.prepare(
                "SELECT id, run_id, created_at, utterance, is_vision
                 FROM conversations
                 ORDER BY created_at DESC
                 LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt.query_map(params![limit, offset], |row| {
                Ok(ConversationSummary {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    created_at: row.get(2)?,
                    utterance: row.get(3)?,
                    is_vision: row.get::<_, i32>(4)? != 0,
                })
            })?;

            let mut result = Vec::new();
            for row in rows {
                result.push(row?);
            }
            Ok(result)
        })
        .await?
    }

    /// Get a single conversation with all its messages
    pub async fn get_conversation(
        &self,
        id: i64,
    ) -> Result<Option<ConversationDetail>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|e| format!("lock: {e}"))?;

            let summary: ConversationSummary = conn
                .query_row(
                    "SELECT id, run_id, created_at, utterance, is_vision
                     FROM conversations WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok(ConversationSummary {
                            id: row.get(0)?,
                            run_id: row.get(1)?,
                            created_at: row.get(2)?,
                            utterance: row.get(3)?,
                            is_vision: row.get::<_, i32>(4)? != 0,
                        })
                    },
                )
                .map_err(|e| format!("conversation not found: {e}"))?;

            let mut stmt = conn.prepare(
                "SELECT role, content, seq
                 FROM conversation_messages
                 WHERE conversation_id = ?1
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                Ok(ConversationMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    seq: row.get(2)?,
                })
            })?;

            let messages: Vec<ConversationMessage> = rows.collect::<Result<_, _>>()?;

            Ok(Some(ConversationDetail {
                id: summary.id,
                run_id: summary.run_id,
                created_at: summary.created_at,
                utterance: summary.utterance,
                is_vision: summary.is_vision,
                messages,
            }))
        })
        .await?
    }
}

/// Convert rig [`Message`] history into `(role, content)` pairs for storage.
fn messages_to_pairs(history: &[Message]) -> Vec<(String, String)> {
    history
        .iter()
        .filter_map(|m| match m {
            Message::User { content } => {
                let text: String = content
                    .iter()
                    .filter_map(|p| match p {
                        UserContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");

                (!text.is_empty()).then(|| ("user".into(), text))
            }
            Message::Assistant { content, .. } => {
                let text: String = content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");

                (!text.is_empty()).then(|| ("assistant".into(), text))
            }
            Message::System { content } => {
                (!content.is_empty()).then(|| ("system".into(), content.clone()))
            }
        })
        .collect()
}

fn status_to_str(status: &MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Pending => "pending",
        MemoryStatus::Uploading => "uploading",
        MemoryStatus::Complete => "complete",
        MemoryStatus::Failed => "failed",
    }
}

fn str_to_status(s: &str) -> MemoryStatus {
    match s {
        "pending" => MemoryStatus::Pending,
        "uploading" => MemoryStatus::Uploading,
        "complete" => MemoryStatus::Complete,
        "failed" => MemoryStatus::Failed,
        other => {
            warn!(
                status = other,
                "unknown memory status, defaulting to Pending"
            );
            MemoryStatus::Pending
        }
    }
}

/// Build a `MemoryRecord` from a row.
fn memory_from_row(row: &rusqlite::Row) -> MemoryRecord {
    let status_str: String = row.get_unwrap(4);
    let lat: Option<f64> = row.get_unwrap(6);
    let lon: Option<f64> = row.get_unwrap(7);

    let location = match (lat, lon) {
        (Some(latitude), Some(longitude)) => Some(Location {
            latitude,
            longitude,
            accuracy: row.get_unwrap::<_, Option<f64>>(8).map(|a| a as f32),
            human_readable: row.get_unwrap(9),
            full_address: row.get_unwrap(10),
        }),
        _ => None,
    };

    MemoryRecord {
        uuid: row.get_unwrap(0),
        memory_type: row.get_unwrap(1),
        device_local_id: row.get_unwrap(2),
        created_at: row.get_unwrap(3),
        status: str_to_status(&status_str),
        files: Vec::new(), // filled in by caller
        thumbnail_count: row.get_unwrap::<_, i64>(5) as usize,
        location,
    }
}

/// Fetch the file list for a given memory UUID.
fn get_files_for_memory(conn: &Connection, uuid: &str) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt =
        conn.prepare("SELECT filename FROM memory_files WHERE memory_uuid = ?1 ORDER BY id")?;
    let rows = stmt.query_map(params![uuid], |row| row.get(0))?;
    rows.collect()
}

/// Fetch a single memory by UUID (with files).
fn get_memory_inner(
    conn: &Connection,
    uuid: &str,
) -> Result<Option<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
    let mut stmt = conn.prepare(
        "SELECT uuid, memory_type, device_local_id, created_at, status,
                thumbnail_count, latitude, longitude, location_accuracy,
                location_human_readable, location_full_address
         FROM memories WHERE uuid = ?1",
    )?;
    let result = stmt
        .query_row(params![uuid], |row| Ok(memory_from_row(row)))
        .ok();

    match result {
        Some(mut record) => {
            record.files = get_files_for_memory(conn, uuid)?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}
