use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// How long a captured image is retained after its last access
const IMAGE_TTL: Duration = Duration::from_secs(300);

/// Hard cap on the number of distinct run images held at once. Oldest entries are evicted first when exceeded
// TODO: This is not purged on a timer
const MAX_IMAGES: usize = 16;

/// A store for captured VLM images, keyed by run id, with automatic eviction of old entries
///
/// This is used to pass images from AnalyzeImage to later Understand calls without needing to resend the image bytes through gRPC
/// or re-capture them on the device
#[derive(Clone)]
pub struct LiveImageStore {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

struct Entry {
    bytes: Vec<u8>,
    last_access: Instant,
}

impl LiveImageStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Store image bytes under a run id
    pub async fn put(&self, run_id: &str, bytes: Vec<u8>) {
        let mut map = self.inner.lock().await;

        prune(&mut map);

        map.insert(
            run_id.to_string(),
            Entry {
                bytes,
                last_access: Instant::now(),
            },
        );
    }

    /// Fetch image bytes for a run id, refreshing its expiry on hit
    pub async fn get_refresh(&self, run_id: &str) -> Option<Vec<u8>> {
        let mut map = self.inner.lock().await;

        prune(&mut map);

        let entry = map.get_mut(run_id)?;
        entry.last_access = Instant::now();

        Some(entry.bytes.clone())
    }
}

/// Drop expired entries and enforce the size cap by evicting the least-recently-accessed entries
fn prune(map: &mut HashMap<String, Entry>) {
    map.retain(|_, entry| entry.last_access.elapsed() < IMAGE_TTL);

    if map.len() <= MAX_IMAGES {
        return;
    }

    let mut ordered: Vec<(String, Instant)> = map
        .iter()
        .map(|(run_id, entry)| (run_id.clone(), entry.last_access))
        .collect();

    ordered.sort_by_key(|(_, last_access)| Reverse(*last_access));

    let keep: HashSet<String> = ordered
        .into_iter()
        .take(MAX_IMAGES)
        .map(|(run_id, _)| run_id)
        .collect();

    map.retain(|run_id, _| keep.contains(run_id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_then_get_returns_bytes() {
        let store = LiveImageStore::new();
        store.put("run-a", vec![1, 2, 3]).await;
        assert_eq!(store.get_refresh("run-a").await, Some(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn miss_returns_none() {
        let store = LiveImageStore::new();
        assert_eq!(store.get_refresh("nope").await, None);
    }

    #[tokio::test]
    async fn get_refresh_can_be_read_repeatedly() {
        let store = LiveImageStore::new();
        store.put("run-a", vec![9]).await;
        assert_eq!(store.get_refresh("run-a").await, Some(vec![9]));
        // A second follow-up against the same image still hits.
        assert_eq!(store.get_refresh("run-a").await, Some(vec![9]));
    }

    #[tokio::test]
    async fn capacity_evicts_least_recently_accessed() {
        let store = LiveImageStore::new();
        // Fill beyond capacity; oldest insert (run-0) is dropped.
        for i in 0..(MAX_IMAGES + 1) {
            store.put(&format!("run-{i}"), vec![i as u8]).await;
        }
        assert_eq!(store.get_refresh("run-0").await, None);
        // The newest entry survives.
        assert_eq!(
            store.get_refresh(&format!("run-{MAX_IMAGES}")).await,
            Some(vec![MAX_IMAGES as u8])
        );
    }

    #[tokio::test]
    async fn expired_entries_are_pruned() {
        let store = LiveImageStore::new();
        store.put("run-a", vec![1]).await;
        // Force the entry's last_access well past the TTL.
        {
            let mut map = store.inner.lock().await;
            if let Some(entry) = map.get_mut("run-a") {
                entry.last_access = Instant::now() - IMAGE_TTL - Duration::from_secs(1);
            }
        }
        assert_eq!(store.get_refresh("run-a").await, None);
    }
}
