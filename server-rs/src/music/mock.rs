//! The built-in mock provider: a fixed set of tracks that all play a local test
//! tone. It backs the shim before any real provider is configured and serves as
//! the reference `MusicProvider` implementation.

use async_trait::async_trait;

use super::{MusicProvider, ProviderTrack};

/// The local test tone served by the shim (`/tidal-shim/audio/tone.wav`).
const TONE_URL: &str = "http://127.0.0.1:8080/tidal-shim/audio/tone.wav";

/// Number of distinct entries in the mock queue.
const QUEUE_SIZE: usize = 6;

pub struct MockProvider;

impl MockProvider {
    fn track(id: impl Into<String>) -> ProviderTrack {
        let id = id.into();
        ProviderTrack {
            title: format!("Penumbra Test Tone ({id})"),
            id,
            artist: "Penumbra".to_string(),
            album: "Shim Mock".to_string(),
            duration_ms: 20_000,
        }
    }
}

#[async_trait]
impl MusicProvider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn queue(&self, limit: usize) -> Vec<ProviderTrack> {
        (1..=limit.min(QUEUE_SIZE))
            .map(|i| Self::track(format!("mock-{i}")))
            .collect()
    }

    async fn search_top(&self, _term: &str) -> Option<ProviderTrack> {
        Some(Self::track("mock-1"))
    }

    async fn track(&self, id: &str) -> ProviderTrack {
        Self::track(id)
    }

    async fn recommendations(&self, _seed_id: &str, limit: usize) -> Vec<ProviderTrack> {
        self.queue(limit).await
    }

    async fn playback(&self, _id: &str) -> String {
        // Every mock track plays the same tone.
        TONE_URL.to_string()
    }
}
