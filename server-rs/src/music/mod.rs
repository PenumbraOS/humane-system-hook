//! The music provider seam.
//!
//! `MusicProvider` is the single boundary every music source implements. The
//! Tidal shim ([`crate::services::tidal_shim`]) talks only to this trait, so a
//! new source (Apple, Tidal, Mopidy, …) is a new `impl` in its own file — no
//! change to the shim or to any core server file. Providers return neutral
//! [`ProviderTrack`] values; translating those into the app's Tidal wire format
//! is the shim's job.
//!
//! Only the built-in [`mock::MockProvider`] exists so far (a local test tone);
//! real providers land in later PRs behind this same trait.

mod mock;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::Config;

/// A track in provider-neutral form. The `id` round-trips back to the provider
/// via [`MusicProvider::playback`], so it can be any provider-local identifier.
#[derive(Debug, Clone)]
pub struct ProviderTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

/// The one interface every music source implements. `Send + Sync` so the shim
/// can hold it as `Arc<dyn MusicProvider>` across async handlers.
#[async_trait]
pub trait MusicProvider: Send + Sync {
    /// Stable short name for logging.
    fn name(&self) -> &'static str;

    /// The default "play music" queue (charts / editorial).
    async fn queue(&self, limit: usize) -> Vec<ProviderTrack>;

    /// The best match for a search term, or `None` if nothing matched.
    async fn search_top(&self, term: &str) -> Option<ProviderTrack>;

    /// Metadata for one track by id.
    async fn track(&self, id: &str) -> ProviderTrack;

    /// Tracks related to a seed, for the device's "up next" / radio queue.
    async fn recommendations(&self, seed_id: &str, limit: usize) -> Vec<ProviderTrack>;

    /// A URL for the track that the device's player fetches; the shim wraps it in
    /// a BTS manifest. (Device-native sources will extend this when they land.)
    async fn playback(&self, id: &str) -> String;
}

/// Shared, cheaply-cloned handle the shim router holds.
pub type SharedProvider = Arc<dyn MusicProvider>;

/// Build the configured provider. Only `mock` exists today; real providers add
/// their own arm here as they land.
pub fn from_config(config: &Config) -> SharedProvider {
    match config.music.provider.as_str() {
        "mock" => Arc::new(mock::MockProvider),
        other => {
            tracing::warn!(provider = other, "unknown music provider, using mock");
            Arc::new(mock::MockProvider)
        }
    }
}
