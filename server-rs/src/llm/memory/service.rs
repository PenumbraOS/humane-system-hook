use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use memvid_core::{AskMode, AskRequest, FrameId, Memvid, VecEmbedder};
use tokio::task;
use tracing::info;

use crate::config::LlmMemoryConfig;
use crate::llm::memory::metadata::MemoryMetadata;
use crate::llm::memory::MemoryKind;
use crate::util::{compact_whitespace, truncate_on_char_boundary};

use super::embedder::MemvidEmbedder;
use super::types::{MemoryRecord, MemorySearchResult};

#[derive(Clone)]
pub struct MemoryService {
    inner: Arc<MemoryInner>,
}

struct MemoryInner {
    path: PathBuf,
    config: LlmMemoryConfig,
    memvid: Mutex<Memvid>,
    embedder: MemvidEmbedder,
}

impl MemoryService {
    pub async fn open(config: LlmMemoryConfig) -> Result<Self, String> {
        if !config.enabled {
            return Err("memory service cannot be opened when disabled".to_string());
        }

        let path = PathBuf::from(&config.path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| format!("failed to create memory directory: {err}"))?;
        }

        let memvid_path = path.clone();
        let memvid = task::spawn_blocking(move || {
            if memvid_path.exists() {
                Memvid::open(memvid_path)
            } else {
                match Memvid::create(&memvid_path) {
                    Ok(memvid) => Ok(memvid),
                    Err(err) => {
                        let _ = std::fs::remove_file(&memvid_path);
                        Err(err)
                    }
                }
            }
        })
        .await
        .map_err(|err| format!("memory open task failed: {err}"))?
        .map_err(|err| err.to_string())?;

        let service = Self {
            inner: Arc::new(MemoryInner {
                path,
                config,
                memvid: Mutex::new(memvid),
                embedder: MemvidEmbedder::new()?,
            }),
        };

        info!(path = %service.inner.path.display(), "assistant memory ready");
        Ok(service)
    }

    pub async fn remember(
        &self,
        text: String,
        kind: MemoryKind,
        importance: f32,
    ) -> Result<MemoryRecord, String> {
        self.with_memvid("memory write", move |memvid, embedder| {
            let metadata = MemoryMetadata::new(kind, importance, Utc::now().to_rfc3339());
            let embedding = embedder.embed_query(&text).map_err(|err| err.to_string())?;

            memvid
                .put_with_embedding_and_options(
                    text.as_bytes(),
                    embedding,
                    metadata.to_put_options(&text),
                )
                .map_err(|err| err.to_string())?;

            memvid.commit().map_err(|err| err.to_string())?;

            Ok(MemoryRecord::from_metadata(
                memvid.next_frame_id(),
                text,
                metadata,
            ))
        })
        .await
    }

    pub async fn search(
        &self,
        query: String,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>, String> {
        let max_limit = self.inner.config.top_k.max(1) * 4;
        let snippet_chars = self.inner.config.snippet_chars;

        self.with_memvid("memory search", move |memvid, embedder| {
            let response = memvid
                .ask(
                    AskRequest {
                        question: query,
                        top_k: limit.clamp(1, max_limit.max(1)),
                        snippet_chars,
                        uri: None,
                        scope: None,
                        cursor: None,
                        start: None,
                        end: None,
                        context_only: true,
                        mode: AskMode::Hybrid,
                        as_of_frame: None,
                        as_of_ts: None,
                        adaptive: None,
                        acl_context: None,
                        acl_enforcement_mode: Default::default(),
                    },
                    Some(embedder),
                )
                .map_err(|err| err.to_string())?;

            let mut seen = HashSet::new();
            let mut results = Vec::new();

            for hit in response.retrieval.hits {
                if !seen.insert(hit.frame_id) {
                    continue;
                }

                let bytes = memvid
                    .frame_canonical_payload(hit.frame_id)
                    .map_err(|err| err.to_string())?;
                let text = String::from_utf8_lossy(&bytes).trim().to_string();

                results.push(MemorySearchResult::from_metadata(
                    hit.frame_id,
                    text,
                    hit.score,
                    hit.metadata.as_ref(),
                ));
            }

            Ok(results)
        })
        .await
    }

    pub async fn retrieve_context(&self, query: String) -> Result<Option<String>, String> {
        if !self.inner.config.auto_retrieve {
            return Ok(None);
        }

        let mut context = String::new();

        let max_chars = self.inner.config.max_context_chars;
        let results = self.search(query, self.inner.config.top_k).await?;

        for result in results {
            let line = format!("- {}\n", compact_whitespace(&result.text));

            context.push_str(&line);

            if context.len() >= max_chars {
                truncate_on_char_boundary(&mut context, max_chars);
                break;
            }
        }

        let context = context.trim();

        if !context.is_empty() {
            Ok(Some(context.to_string()))
        } else {
            Ok(None)
        }
    }

    pub async fn update(
        &self,
        id: FrameId,
        text: String,
        kind: Option<MemoryKind>,
        importance: Option<f32>,
    ) -> Result<MemoryRecord, String> {
        self.with_memvid("memory update", move |memvid, embedder| {
            let frame = memvid.frame_by_id(id).map_err(|err| err.to_string())?;

            let mut metadata = MemoryMetadata::from_extra_metadata(&frame.extra_metadata);
            metadata.updated_at = Utc::now().to_rfc3339();

            if let Some(kind) = kind {
                metadata.kind = kind;
            }

            if let Some(importance) = importance {
                metadata.importance = importance.clamp(0.0, 1.0);
            }

            let embedding = embedder.embed_query(&text).map_err(|err| err.to_string())?;

            memvid
                .update_frame(
                    id,
                    Some(text.clone().into_bytes()),
                    metadata.to_put_options(&text),
                    Some(embedding),
                )
                .map_err(|err| err.to_string())?;

            memvid.commit().map_err(|err| err.to_string())?;

            Ok(MemoryRecord::from_metadata(id, text, metadata))
        })
        .await
    }

    pub async fn forget(&self, frame_id: FrameId) -> Result<(), String> {
        self.with_memvid("memory delete", move |memvid, _embedder| {
            memvid
                .delete_frame(frame_id)
                .map_err(|err| err.to_string())?;

            memvid.commit().map_err(|err| err.to_string())
        })
        .await
    }

    async fn with_memvid<T, F>(&self, label: &'static str, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Memvid, &MemvidEmbedder) -> Result<T, String> + Send + 'static,
    {
        let inner = self.inner.clone();
        task::spawn_blocking(move || {
            let mut memvid = inner
                .memvid
                .lock()
                .map_err(|err| format!("memory lock poisoned: {err}"))?;

            f(&mut memvid, &inner.embedder)
        })
        .await
        .map_err(|err| format!("{label} task failed: {err}"))?
    }
}
