//! Tidal backend — proxies real `api.tidal.com` so the device's *native* Tidal
//! player handles playback.
//!
//! The Ai Pin's music app is itself a Tidal client (the shim is Tidal-shaped);
//! we normally redirect `api.tidal.com` → the shim and fake the responses. For
//! the Tidal provider we instead forward to real Tidal with the user's OAuth
//! token and pass the real `playbackinfopostpaywall` response through — its BTS
//! manifest points at Tidal's CDN with a `keyId`, and the device decrypts +
//! plays it natively. So there's no embedded player, no server-side streaming,
//! and no DRM work on our side.
//!
//! Auth is Tidal's OAuth **device-authorization** flow (show the user a code +
//! `link.tidal.com`), then a refresh token we persist. `client_id`/`_secret`
//! are the reverse-engineered Tidal app credentials the user supplies.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::music::ProviderTrack;

const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";
const API_BASE: &str = "https://api.tidal.com/v1";
const SCOPE: &str = "r_usr w_usr w_sub";

/// The pending device-flow login the user must complete at `link.tidal.com`.
#[derive(Debug, Clone)]
pub struct DeviceLogin {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

pub struct TidalClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    refresh_token: Option<String>,
    country_code: String,
    quality: String,
    access: Mutex<Option<CachedToken>>,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct DeviceAuthResponse {
    #[serde(rename = "deviceCode")]
    device_code: String,
    #[serde(rename = "userCode")]
    user_code: String,
    #[serde(rename = "verificationUri", default)]
    verification_uri: String,
    #[serde(rename = "verificationUriComplete", default)]
    verification_uri_complete: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(rename = "expiresIn", default)]
    expires_in: u64,
}
fn default_interval() -> u64 {
    2
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

// ---- catalog models (Tidal /v1) ----
#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    tracks: Option<TrackList>,
}
#[derive(Deserialize, Default)]
struct TrackList {
    #[serde(default)]
    items: Vec<TidalTrack>,
}
#[derive(Deserialize)]
struct TidalTrack {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    duration: u64,
    #[serde(default)]
    artist: Option<TidalNamed>,
    #[serde(default)]
    artists: Vec<TidalNamed>,
    #[serde(default)]
    album: Option<TidalNamed>,
}
#[derive(Deserialize, Default)]
struct TidalNamed {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl TidalTrack {
    fn into_provider(self) -> ProviderTrack {
        // Tidal search returns the artist under `artists[]`; older responses use
        // the singular `artist`. Prefer whichever has a name.
        let artist = self
            .artists
            .into_iter()
            .find_map(|a| a.name)
            .or_else(|| self.artist.and_then(|a| a.name))
            .unwrap_or_default();
        ProviderTrack {
            id: self.id.to_string(),
            title: self.title,
            artist,
            album: self.album.and_then(|a| a.title).unwrap_or_default(),
            duration_ms: self.duration.saturating_mul(1000),
        }
    }
}

impl TidalClient {
    pub fn new(
        http: reqwest::Client,
        client_id: String,
        client_secret: String,
        refresh_token: Option<String>,
        country_code: String,
        quality: String,
    ) -> Self {
        Self {
            http,
            client_id,
            client_secret,
            refresh_token: refresh_token.filter(|t| !t.trim().is_empty()),
            country_code,
            quality,
            access: Mutex::new(None),
        }
    }

    /// Whether a user is signed in (has a refresh token).
    pub fn has_user_auth(&self) -> bool {
        self.refresh_token.is_some()
    }

    // ── OAuth device-authorization flow ──────────────────────────────────

    /// Start a device-flow login; the user visits `verification_uri` and enters
    /// `user_code`. Poll [`poll_login`] with the returned `device_code`.
    pub async fn start_device_login(&self) -> Result<DeviceLogin, String> {
        let resp = self
            .http
            .post(format!("{AUTH_BASE}/device_authorization"))
            .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPE)])
            .send()
            .await
            .map_err(|e| format!("tidal device auth failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("tidal device auth {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        let d: DeviceAuthResponse = resp.json().await.map_err(|e| format!("tidal device auth decode: {e}"))?;
        let uri = if !d.verification_uri_complete.is_empty() {
            if d.verification_uri_complete.starts_with("http") {
                d.verification_uri_complete
            } else {
                format!("https://{}", d.verification_uri_complete)
            }
        } else {
            d.verification_uri
        };
        Ok(DeviceLogin {
            device_code: d.device_code,
            user_code: d.user_code,
            verification_uri: uri,
            interval: d.interval,
            expires_in: d.expires_in,
        })
    }

    /// Poll once for the device-flow result. Returns `Ok(Some(refresh_token))`
    /// once the user authorizes, `Ok(None)` while still pending, `Err` on a hard
    /// failure (expired/denied).
    pub async fn poll_login(&self, device_code: &str) -> Result<Option<String>, String> {
        let resp = self
            .http
            .post(format!("{AUTH_BASE}/token"))
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("scope", SCOPE),
            ])
            .send()
            .await
            .map_err(|e| format!("tidal token poll failed: {e}"))?;
        if resp.status().is_success() {
            let t: TokenResponse = resp.json().await.map_err(|e| format!("tidal token decode: {e}"))?;
            return Ok(t.refresh_token.filter(|r| !r.is_empty()));
        }
        // 400 with authorization_pending means keep polling; anything else is fatal.
        let text = resp.text().await.unwrap_or_default();
        if text.contains("authorization_pending") || text.contains("pending") {
            Ok(None)
        } else {
            Err(format!("tidal token: {text}"))
        }
    }

    /// A cached user access token, minted from the refresh token.
    async fn access_token(&self) -> Result<String, String> {
        let refresh = self
            .refresh_token
            .as_deref()
            .ok_or_else(|| "tidal: not signed in".to_string())?;
        let mut guard = self.access.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at > Instant::now() {
                return Ok(cached.value.clone());
            }
        }
        let resp = self
            .http
            .post(format!("{AUTH_BASE}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("tidal refresh failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("tidal refresh {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        let t: TokenResponse = resp.json().await.map_err(|e| format!("tidal refresh decode: {e}"))?;
        let ttl = t.expires_in.saturating_sub(60).max(60);
        *guard = Some(CachedToken {
            value: t.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(t.access_token)
    }

    async fn get_json(&self, path: &str, extra: &[(&str, &str)]) -> Result<Value, String> {
        let token = self.access_token().await?;
        let mut params: Vec<(&str, &str)> = vec![("countryCode", self.country_code.as_str())];
        params.extend_from_slice(extra);
        let url = reqwest::Url::parse_with_params(&format!("{API_BASE}{path}"), &params)
            .map_err(|e| format!("tidal url build failed: {e}"))?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("tidal request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("tidal {path} {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        resp.json().await.map_err(|e| format!("tidal decode failed: {e}"))
    }

    // ── catalog ──────────────────────────────────────────────────────────

    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let limit = limit.to_string();
        let body: SearchResponse = serde_json::from_value(
            self.get_json("/search", &[("query", term), ("types", "TRACKS"), ("limit", &limit)]).await?,
        )
        .map_err(|e| format!("tidal search decode: {e}"))?;
        Ok(body
            .tracks
            .map(|t| t.items.into_iter().map(TidalTrack::into_provider).collect())
            .unwrap_or_default())
    }

    pub async fn search_top(&self, term: &str) -> Result<Option<ProviderTrack>, String> {
        Ok(self.search_songs(term, 1).await?.into_iter().next())
    }

    pub async fn queue(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        // No editorial endpoint wired yet; a broad recent search as a stand-in.
        self.search_songs("top hits", limit).await
    }

    pub async fn track(&self, id: &str) -> Result<ProviderTrack, String> {
        let v = self.get_json(&format!("/tracks/{id}"), &[]).await?;
        let t: TidalTrack = serde_json::from_value(v).map_err(|e| format!("tidal track decode: {e}"))?;
        Ok(t.into_provider())
    }

    /// The real Tidal `playbackinfopostpaywall` response for a track, forwarded
    /// verbatim to the device — its BTS manifest (real Tidal CDN URLs + keyId)
    /// is what the device's native Tidal player decrypts and plays.
    pub async fn playback_info(&self, id: &str) -> Result<Value, String> {
        self.get_json(
            &format!("/tracks/{id}/playbackinfopostpaywall"),
            &[
                ("playbackmode", "STREAM"),
                ("assetpresentation", "FULL"),
                ("audioquality", self.quality.as_str()),
            ],
        )
        .await
    }
}
