//! Filesystem-backed media storage for captured photos, videos, and notes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

// ─── Memory metadata ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub uuid: String,
    pub memory_type: String,
    pub device_local_id: String,
    pub created_at: String,
    pub status: MemoryStatus,
    /// Filenames the server told the device to upload.
    pub files: Vec<String>,
    /// Number of thumbnails saved.
    pub thumbnail_count: usize,
    /// Plaintext location (decoded from LocationEnvelope proto when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Pending,
    Uploading,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_readable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_address: Option<String>,
}

// ─── MediaStore ─────────────────────────────────────────────────────

pub struct MediaStore {
    base_dir: PathBuf,
    memories: Vec<MemoryRecord>,
}

impl MediaStore {
    /// Open (or create) the media store at the given directory.
    pub async fn open(base_dir: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let base_dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(&base_dir).await?;

        let index_path = base_dir.join("memories.json");
        let memories = if index_path.exists() {
            let data = fs::read_to_string(&index_path).await?;
            serde_json::from_str(&data).unwrap_or_else(|e| {
                warn!("failed to parse memories.json, starting fresh: {e}");
                Vec::new()
            })
        } else {
            Vec::new()
        };

        info!(dir = %base_dir.display(), count = memories.len(), "media store opened");
        Ok(Self { base_dir, memories })
    }

    /// Persist the index to disk.
    async fn flush(&self) -> Result<(), Box<dyn std::error::Error>> {
        let data = serde_json::to_string_pretty(&self.memories)?;
        let path = self.base_dir.join("memories.json");
        fs::write(&path, data).await?;
        Ok(())
    }

    /// Create a new memory record and its directory.
    pub async fn create_memory(
        &mut self,
        uuid: String,
        memory_type: &str,
        device_local_id: &str,
        created_at: &str,
        files: Vec<String>,
        location: Option<Location>,
    ) -> Result<&MemoryRecord, Box<dyn std::error::Error>> {
        let dir = self.base_dir.join(&uuid);
        fs::create_dir_all(&dir).await?;

        let record = MemoryRecord {
            uuid: uuid.clone(),
            memory_type: memory_type.to_string(),
            device_local_id: device_local_id.to_string(),
            created_at: created_at.to_string(),
            status: MemoryStatus::Pending,
            files,
            thumbnail_count: 0,
            location,
        };
        self.memories.push(record);
        self.flush().await?;
        Ok(self.memories.last().unwrap())
    }

    /// Save a thumbnail as a separate .jpg file.
    pub async fn save_thumbnail(
        &mut self,
        uuid: &str,
        index: usize,
        data: &[u8],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let filename = format!("thumbnail_{index}.jpg");
        let path = self.base_dir.join(uuid).join(&filename);
        fs::write(&path, data).await?;

        // Update thumbnail count and add to files list
        if let Some(rec) = self.memories.iter_mut().find(|m| m.uuid == uuid) {
            rec.thumbnail_count = rec.thumbnail_count.max(index + 1);
            if !rec.files.contains(&filename) {
                rec.files.push(filename.clone());
            }
            self.flush().await?;
        }

        info!(uuid, filename, bytes = data.len(), "saved thumbnail");
        Ok(filename)
    }

    /// Save an uploaded file (streamed from HTTP PUT).
    pub async fn save_upload(
        &self,
        uuid: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = self.base_dir.join(uuid);
        if !dir.exists() {
            return Err(format!("memory directory not found: {uuid}").into());
        }
        let path = dir.join(filename);
        let mut file = fs::File::create(&path).await?;
        file.write_all(data).await?;
        file.flush().await?;
        info!(uuid, filename, bytes = data.len(), "saved upload");
        Ok(())
    }

    /// Mark a memory as complete.
    pub async fn complete_memory(
        &mut self,
        uuid: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if let Some(rec) = self.memories.iter_mut().find(|m| m.uuid == uuid) {
            rec.status = MemoryStatus::Complete;
            self.flush().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Mark a memory as failed.
    pub async fn fail_memory(&mut self, uuid: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(rec) = self.memories.iter_mut().find(|m| m.uuid == uuid) {
            rec.status = MemoryStatus::Failed;
            self.flush().await?;
        }
        Ok(())
    }

    /// Delete a memory (record + directory).
    pub async fn delete_memory(&mut self, uuid: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let dir = self.base_dir.join(uuid);
        if dir.exists() {
            fs::remove_dir_all(&dir).await?;
        }
        let before = self.memories.len();
        self.memories.retain(|m| m.uuid != uuid);
        let removed = self.memories.len() < before;
        if removed {
            self.flush().await?;
        }
        Ok(removed)
    }

    /// Look up a memory by UUID.
    pub fn get_memory(&self, uuid: &str) -> Option<&MemoryRecord> {
        self.memories.iter().find(|m| m.uuid == uuid)
    }

    /// Find a memory that owns a given filename.
    pub fn find_memory_for_file(&self, filename: &str) -> Option<&MemoryRecord> {
        self.memories
            .iter()
            .find(|m| m.files.iter().any(|f| f == filename))
    }

    /// List all memories.
    pub fn list_memories(&self) -> &[MemoryRecord] {
        &self.memories
    }

    /// Base directory path.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}
