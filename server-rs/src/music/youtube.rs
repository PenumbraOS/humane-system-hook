//! YouTube Music provider.
//!
//! Talks to the LAN companion resolver (`tools/ytmusic-resolver.py`), which
//! wraps `yt-dlp`/`ffmpeg`: JSON metadata for search/queue/track/recommendations,
//! and a range-capable WAV stream at `/stream/{id}`. The provider itself does no
//! media work — [`playback`](YouTubeMusicProvider::playback) just hands the shim
//! the resolver's stream URL, which the device player fetches directly.

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder, Url};
use serde::Deserialize;
use tracing::warn;

use super::{MusicProvider, ProviderTrack};
use crate::config::Config;

/// Resolver JSON shape for a single track. Field names match the resolver's
/// output; extra fields are ignored.
#[derive(Debug, Deserialize)]
struct ResolverTrack {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    duration_ms: u64,
}

impl From<ResolverTrack> for ProviderTrack {
    fn from(t: ResolverTrack) -> Self {
        ProviderTrack {
            id: t.id,
            title: t.title,
            artist: t.artist,
            album: t.album,
            duration_ms: t.duration_ms,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TrackList {
    #[serde(default)]
    tracks: Vec<ResolverTrack>,
}

pub struct YouTubeMusicProvider {
    /// Resolver base URL, without a trailing slash.
    base_url: String,
    http: Client,
}

impl YouTubeMusicProvider {
    pub fn from_config(config: &Config) -> Self {
        Self {
            base_url: config.music.resolver_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Send a request expecting a `{ "tracks": [...] }` body; empty on any
    /// failure (logged).
    async fn fetch_list(&self, url: &str, req: RequestBuilder) -> Vec<ProviderTrack> {
        match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(resp) => match resp.json::<TrackList>().await {
                Ok(list) => list.tracks.into_iter().map(Into::into).collect(),
                Err(error) => {
                    warn!(%url, %error, "youtube resolver: bad track list JSON");
                    Vec::new()
                }
            },
            Err(error) => {
                warn!(%url, %error, "youtube resolver: list request failed");
                Vec::new()
            }
        }
    }
}

#[async_trait]
impl MusicProvider for YouTubeMusicProvider {
    fn name(&self) -> &'static str {
        "youtube"
    }

    async fn queue(&self, limit: usize) -> Vec<ProviderTrack> {
        let url = self.url("/queue");
        let mut tracks = self.fetch_list(&url, self.http.get(&url)).await;
        tracks.truncate(limit);
        tracks
    }

    async fn search_top(&self, term: &str) -> Option<ProviderTrack> {
        // `parse_with_params` percent-encodes the value, so arbitrary terms are safe.
        let parsed = match Url::parse_with_params(&self.url("/search"), &[("q", term)]) {
            Ok(url) => url,
            Err(error) => {
                warn!(%term, %error, "youtube resolver: could not build search url");
                return None;
            }
        };
        let url = parsed.to_string();
        self.fetch_list(&url, self.http.get(parsed))
            .await
            .into_iter()
            .next()
    }

    async fn search_many(&self, term: &str, limit: usize) -> Vec<ProviderTrack> {
        let parsed = match Url::parse_with_params(&self.url("/search"), &[("q", term)]) {
            Ok(url) => url,
            Err(error) => {
                warn!(%term, %error, "youtube resolver: could not build search url");
                return Vec::new();
            }
        };
        let url = parsed.to_string();
        let mut tracks = self.fetch_list(&url, self.http.get(parsed)).await;
        tracks.truncate(limit);
        tracks
    }

    async fn track(&self, id: &str) -> ProviderTrack {
        // YouTube video ids are URL-safe (`[A-Za-z0-9_-]{11}`), so the id can go
        // straight into the path segment.
        let url = self.url(&format!("/track/{id}"));
        match self
            .http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.json::<ResolverTrack>().await {
                Ok(track) => return track.into(),
                Err(error) => warn!(%url, %error, "youtube resolver: bad track JSON"),
            },
            Err(error) => warn!(%url, %error, "youtube resolver: track request failed"),
        }
        // Fallback so playback still works even if metadata lookup failed: the
        // id round-trips to `playback()` unchanged.
        ProviderTrack {
            id: id.to_string(),
            title: id.to_string(),
            artist: "YouTube Music".to_string(),
            album: "YouTube Music".to_string(),
            duration_ms: 0,
        }
    }

    async fn recommendations(&self, seed_id: &str, limit: usize) -> Vec<ProviderTrack> {
        let url = self.url(&format!("/recommendations/{seed_id}"));
        let mut tracks = self.fetch_list(&url, self.http.get(&url)).await;
        tracks.truncate(limit);
        tracks
    }

    async fn playback(&self, id: &str) -> String {
        self.url(&format!("/stream/{id}"))
    }
}
