//! Spotify Web API client (catalog search + metadata).
//!
//! Uses the client-credentials flow (app-level `client_id`/`client_secret`, no
//! user login) which still works after Spotify's Nov-2024 API restrictions for
//! search and get-track. Recommendations / featured-playlists / new-releases are
//! deprecated for new apps, so the "play music" queue is built from a broad
//! search instead. Playback of the resulting tracks is handled separately by the
//! server's streaming layer (librespot, behind the `spotify-playback` feature).

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::music::{ProviderEntity, ProviderTrack};

const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const API_BASE: &str = "https://api.spotify.com/v1";

struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// Client-credentials Spotify Web API client with a cached bearer token.
pub struct SpotifyClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    market: String,
    token: Mutex<Option<CachedToken>>,
    /// Refresh token from a user's OAuth (PKCE) sign-in, enabling `/me` library
    /// access. Absent = catalog/search only.
    refresh_token: Option<String>,
    user_token: Mutex<Option<CachedToken>>,
    /// `streaming`-scoped token for librespot playback (Premium). Read by the
    /// shim's `spotify-playback` stream path; unused otherwise.
    #[cfg_attr(not(feature = "spotify-playback"), allow(dead_code))]
    streaming_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
    /// Present on the authorization_code exchange (and sometimes on refresh).
    #[serde(default)]
    refresh_token: Option<String>,
}

// ---- User library (`/me`) responses ----

/// Spotify caps page size at 50 and returns a `next` URL for the following page.
const PAGE_SIZE: u32 = 50;
/// Safety cap on pagination so a huge library can't spin indefinitely.
const MAX_PAGES: u32 = 8;

#[derive(Deserialize, Default)]
struct SavedTracksResponse {
    #[serde(default)]
    items: Vec<SavedTrackItem>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct SavedTrackItem {
    #[serde(default)]
    track: Option<TrackObject>,
}

#[derive(Deserialize, Default)]
struct PlaylistsResponse {
    #[serde(default)]
    items: Vec<PlaylistObject>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct PlaylistObject {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tracks: Option<PlaylistTracksMeta>,
}

#[derive(Deserialize)]
struct PlaylistTracksMeta {
    #[serde(default)]
    total: u32,
}

#[derive(Deserialize, Default)]
struct PlaylistTracksResponse {
    #[serde(default)]
    items: Vec<SavedTrackItem>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    tracks: Option<TrackPage>,
}

#[derive(Deserialize, Default)]
struct TrackPage {
    #[serde(default)]
    items: Vec<TrackObject>,
}

#[derive(Deserialize)]
struct TrackObject {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<ArtistObject>,
    #[serde(default)]
    album: Option<AlbumObject>,
    #[serde(rename = "duration_ms", default)]
    duration_ms: u64,
}

#[derive(Deserialize)]
struct ArtistObject {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct AlbumObject {
    #[serde(default)]
    name: String,
}

impl TrackObject {
    fn into_track(self) -> ProviderTrack {
        ProviderTrack {
            artist: self.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            album: self.album.map(|a| a.name).unwrap_or_default(),
            title: self.name,
            duration_ms: self.duration_ms,
            id: self.id,
        }
    }
}

impl SpotifyClient {
    pub fn new(
        http: reqwest::Client,
        client_id: String,
        client_secret: String,
        market: String,
    ) -> Self {
        Self {
            http,
            client_id,
            client_secret,
            market,
            token: Mutex::new(None),
            refresh_token: None,
            user_token: Mutex::new(None),
            streaming_token: None,
        }
    }

    /// Attach a user OAuth refresh token, enabling `/me` library access.
    pub fn with_refresh_token(mut self, refresh_token: Option<String>) -> Self {
        self.refresh_token = refresh_token.filter(|t| !t.trim().is_empty());
        self
    }

    /// Attach a `streaming`-scoped token for librespot playback.
    pub fn with_streaming_token(mut self, token: Option<String>) -> Self {
        self.streaming_token = token.filter(|t| !t.trim().is_empty());
        self
    }

    /// The configured librespot streaming token, if any.
    #[cfg_attr(not(feature = "spotify-playback"), allow(dead_code))]
    pub fn streaming_token(&self) -> Option<&str> {
        self.streaming_token.as_deref()
    }

    /// Whether a user is signed in (real library/favorites available).
    pub fn has_user_auth(&self) -> bool {
        self.refresh_token.is_some()
    }

    /// Top track for a search phrase.
    pub async fn search_top(&self, term: &str) -> Result<Option<ProviderTrack>, String> {
        let page = self.search(term, 1).await?;
        Ok(page.into_iter().next())
    }

    /// Search catalog tracks (used for artist/album/genre intents).
    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        self.search(term, limit.max(1)).await
    }

    /// A "play music" queue. Featured/new-releases are deprecated for new apps,
    /// so this is a broad recent-tracks search.
    pub async fn queue(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        self.search("year:2024-2026", limit).await
    }

    async fn search(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let token = self.token().await?;
        let url = reqwest::Url::parse_with_params(
            &format!("{API_BASE}/search"),
            &[
                ("q", term),
                ("type", "track"),
                ("market", &self.market),
                ("limit", &limit.to_string()),
            ],
        )
        .map_err(|e| format!("spotify url: {e}"))?;
        let body: SearchResponse = self.get_json(url.as_str(), &token).await?;
        Ok(body
            .tracks
            .map(|p| p.items.into_iter().map(TrackObject::into_track).collect())
            .unwrap_or_default())
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        token: &str,
    ) -> Result<T, String> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("spotify request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("spotify {status}: {text}"));
        }
        resp.json::<T>()
            .await
            .map_err(|e| format!("spotify decode failed: {e}"))
    }

    /// Cached client-credentials bearer token, refreshed ~30s before expiry.
    async fn token(&self) -> Result<String, String> {
        let mut guard = self.token.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at > Instant::now() {
                return Ok(cached.value.clone());
            }
        }
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.client_id, self.client_secret));
        let resp = self
            .http
            .post(TOKEN_URL)
            .header("Authorization", format!("Basic {basic}"))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|e| format!("spotify token request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("spotify token {status}: {text}"));
        }
        let token: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("spotify token decode failed: {e}"))?;
        let ttl = token.expires_in.saturating_sub(30).max(60);
        *guard = Some(CachedToken {
            value: token.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(token.access_token)
    }

    /// A cached user access token, minted from the stored refresh token (PKCE
    /// refresh grant — client_id only, no secret).
    async fn user_access_token(&self) -> Result<String, String> {
        let Some(refresh) = self.refresh_token.as_deref() else {
            return Err("spotify: no user sign-in (refresh token) configured".to_string());
        };
        let mut guard = self.user_token.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at > Instant::now() {
                return Ok(cached.value.clone());
            }
        }
        let resp = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", self.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("spotify user token request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("spotify user token {status}: {text}"));
        }
        let token: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("spotify user token decode failed: {e}"))?;
        let ttl = token.expires_in.saturating_sub(30).max(60);
        *guard = Some(CachedToken {
            value: token.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(token.access_token)
    }

    /// The user's saved/liked tracks (`/me/tracks`) — "play my favorites".
    /// Paginates (50/page, following `next`) up to `limit`.
    pub async fn library_tracks(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let token = self.user_access_token().await?;
        let mut out = Vec::new();
        let mut url = format!("{API_BASE}/me/tracks?limit={PAGE_SIZE}");
        for _ in 0..MAX_PAGES {
            let body: SavedTracksResponse = self.get_json(&url, &token).await?;
            out.extend(body.items.into_iter().filter_map(|i| i.track.map(TrackObject::into_track)));
            match body.next {
                Some(next) if (out.len() as u32) < limit => url = next,
                _ => break,
            }
        }
        out.truncate(limit as usize);
        Ok(out)
    }

    /// The user's playlists (`/me/playlists`) — "play my playlists". Paginated.
    pub async fn library_playlists(&self, limit: u32) -> Result<Vec<ProviderEntity>, String> {
        let token = self.user_access_token().await?;
        let mut out = Vec::new();
        let mut url = format!("{API_BASE}/me/playlists?limit={PAGE_SIZE}");
        for _ in 0..MAX_PAGES {
            let body: PlaylistsResponse = self.get_json(&url, &token).await?;
            out.extend(body.items.into_iter().map(|p| ProviderEntity {
                id: p.id,
                name: p.name,
                track_count: p.tracks.map(|t| t.total).unwrap_or(1).max(1),
            }));
            match body.next {
                Some(next) if (out.len() as u32) < limit => url = next,
                _ => break,
            }
        }
        out.truncate(limit as usize);
        Ok(out)
    }

    /// A user playlist's tracks (`/playlists/{id}/tracks`). Paginated.
    pub async fn library_playlist_tracks(
        &self,
        playlist_id: &str,
        limit: u32,
    ) -> Result<Vec<ProviderTrack>, String> {
        let token = self.user_access_token().await?;
        let mut out = Vec::new();
        let mut url =
            format!("{API_BASE}/playlists/{playlist_id}/tracks?limit={PAGE_SIZE}");
        for _ in 0..MAX_PAGES {
            let body: PlaylistTracksResponse = self.get_json(&url, &token).await?;
            out.extend(body.items.into_iter().filter_map(|i| i.track.map(TrackObject::into_track)));
            match body.next {
                Some(next) if (out.len() as u32) < limit => url = next,
                _ => break,
            }
        }
        out.truncate(limit as usize);
        Ok(out)
    }
}

/// Exchange an OAuth authorization code (PKCE) for tokens, returning the refresh
/// token to persist. Used by the Center "Connect Spotify" flow: the browser does
/// the authorize step, the Pin does this exchange so the tokens never leave the
/// device. Client-id + code_verifier only — no client secret (PKCE).
pub async fn exchange_code(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("spotify code exchange failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("spotify code exchange {status}: {text}"));
    }
    let token: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("spotify code exchange decode failed: {e}"))?;
    token
        .refresh_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "spotify: no refresh token in exchange response".to_string())
}
