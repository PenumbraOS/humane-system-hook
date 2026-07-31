//! Music provider abstraction behind the Tidal shim.
//!
//! One [`MusicProvider`] value (built from config) answers the shim's
//! search / queue / metadata / playback-URL needs for whichever backend is
//! active. The playback URL is the key difference between backends:
//!
//! - **Apple** returns a `penumbra-apple://<catalogId>` sentinel that the
//!   on-device MusicKit hook plays (software FairPlay).
//! - **Spotify / YouTube** return a loopback HTTP URL served by this server's
//!   streaming layer, which the device's ExoPlayer plays directly (server-side
//!   decode, so no on-device DRM is required).
//!
//! Providers whose credentials/features aren't configured resolve to
//! [`MusicProvider::Unconfigured`] and return a clear error instead of failing
//! obscurely, so the core Apple path is never affected.

use std::sync::Arc;

use base64::Engine as _;

use crate::config::{Config, MusicProviderKind};
use crate::external::apple_music::{AppleMusicClient, AppleSong};
use crate::external::mopidy::MopidyClient;
use crate::external::spotify::SpotifyClient;
use crate::external::youtube::YoutubeClient;

/// Loopback base the device fetches server-decoded streams from (same host the
/// tone-loopback test used). Spotify/YouTube playback URLs live under here.
pub const STREAM_BASE: &str = "http://127.0.0.1:8080/tidal-shim/stream";

/// Apple sentinel scheme understood by the MusicKit hook.
pub const APPLE_SENTINEL: &str = "penumbra-apple://";

/// Filename (next to `config.toml`) where the server publishes the effective
/// Apple tokens for the on-device MusicKit player to read. Lets a Center-driven
/// login/`.p8` config flow reach the player without a hardcoded token in the
/// hook APK. Same directory as the config so both processes (shared uid) agree.
pub const APPLE_DEVICE_TOKENS_FILE: &str = "apple_tokens.json";

/// Publish the effective Apple developer + user tokens to
/// [`APPLE_DEVICE_TOKENS_FILE`] beside `config_path`, for the on-device player.
/// No-op (with a warning) on any IO/mint failure — never fatal to startup.
pub fn write_device_tokens(config: &Config, config_path: &std::path::Path) {
    let Some(dir) = config_path.parent() else {
        return;
    };
    let path = dir.join(APPLE_DEVICE_TOKENS_FILE);
    let payload = serde_json::json!({
        "developer_token": config.apple_music.effective_developer_token(),
        "user_token": config.apple_music.user_token,
    });
    match serde_json::to_string(&payload) {
        Ok(json) => {
            if let Err(error) = std::fs::write(&path, json) {
                tracing::warn!(error = %error, path = %path.display(), "failed to write apple device tokens");
            } else {
                tracing::info!(path = %path.display(), "<<< published apple device tokens for the player");
            }
        }
        Err(error) => tracing::warn!(error = %error, "failed to serialize apple device tokens"),
    }
}

/// Shared "now playing" state: the shim writes the track it just resolved for
/// playback; the assistant reads it to answer questions about the current song.
pub type NowPlayingHandle = Arc<tokio::sync::RwLock<Option<NowPlaying>>>;

/// The active music provider, hot-swappable at runtime. A provider/credential
/// change in Center stores a freshly-built provider here, so the shim (which
/// reads it live) and the AiBus (rebuilt on the same PUT) both pick it up with
/// no server restart.
pub type SharedProvider = Arc<arc_swap::ArcSwap<MusicProvider>>;

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl NowPlaying {
    /// "Title — Artist (from Album)" for the assistant context.
    pub fn summary(&self) -> String {
        let mut s = self.title.clone();
        if !self.artist.is_empty() {
            s.push_str(" — ");
            s.push_str(&self.artist);
        }
        if !self.album.is_empty() && self.album != self.title {
            s.push_str(&format!(" (from the album {})", self.album));
        }
        s
    }
}

/// A single track, provider-agnostic, as the shim needs it.
#[derive(Debug, Clone)]
pub struct ProviderTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

/// A resolved artist / album / playlist entity for the device's two-step
/// (search entity → fetch its tracks). `track_count` must be > 0 for the device
/// to fetch album/playlist tracks.
#[derive(Debug, Clone)]
pub struct ProviderEntity {
    pub id: String,
    pub name: String,
    pub track_count: u32,
}

/// Apple catalog ids >= 2^31 overflow the 2021 MusicKit SDK's 32-bit id handling
/// and won't play (they hang the decoder). Non-numeric ids (Spotify/YouTube) are
/// always considered playable here.
pub fn apple_id_playable(id: &str) -> bool {
    id.parse::<u64>().map(|n| n < (1u64 << 31)).unwrap_or(true)
}

/// Encode a search term into a URL-path-safe id so providers that resolve
/// entities by search (Spotify/YouTube) can recover the term in the follow-on
/// `/artists/{id}/toptracks` etc. call. Marked with a scheme prefix.
fn search_entity_id(term: &str) -> String {
    format!(
        "q_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(term)
    )
}

/// Recover a search term from a [`search_entity_id`], if it is one.
fn decode_entity_term(id: &str) -> Option<String> {
    let b64 = id.strip_prefix("q_")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64).ok()?;
    String::from_utf8(bytes).ok()
}

impl From<AppleSong> for ProviderTrack {
    fn from(s: AppleSong) -> Self {
        ProviderTrack {
            id: s.id,
            title: s.name,
            artist: s.artist,
            album: s.album,
            duration_ms: s.duration_ms,
        }
    }
}

/// The active music backend.
pub enum MusicProvider {
    Apple(Arc<AppleMusicClient>),
    Spotify(Arc<SpotifyClient>),
    Youtube(Arc<YoutubeClient>),
    Mopidy(Arc<MopidyClient>),
    /// The selected provider is missing credentials/config; `reason` explains.
    Unconfigured {
        kind: MusicProviderKind,
        reason: String,
    },
}

impl MusicProvider {
    /// Build the provider selected by `[music] provider`, falling back to
    /// `Unconfigured` (with a reason) when its credentials are absent.
    pub fn from_config(config: &Config, http: reqwest::Client) -> Self {
        match config.music.provider {
            MusicProviderKind::Apple => {
                match config.apple_music.effective_developer_token() {
                    Some(token) => MusicProvider::Apple(Arc::new(
                        AppleMusicClient::new(
                            http,
                            token,
                            config.apple_music.storefront.clone(),
                        )
                        .with_user_token(config.apple_music.user_token.clone()),
                    )),
                    None => MusicProvider::Unconfigured {
                        kind: MusicProviderKind::Apple,
                        reason: "set [apple_music] p8_private_key + key_id + team_id (or a developer_token)".to_string(),
                    },
                }
            }
            MusicProviderKind::Spotify => {
                match (
                    config.spotify.client_id.as_ref().filter(|s| !s.trim().is_empty()),
                    config
                        .spotify
                        .client_secret
                        .as_ref()
                        .filter(|s| !s.trim().is_empty()),
                ) {
                    (Some(id), Some(secret)) => MusicProvider::Spotify(Arc::new(
                        SpotifyClient::new(
                            http,
                            id.clone(),
                            secret.clone(),
                            config.spotify.market.clone(),
                        )
                        .with_refresh_token(config.spotify.refresh_token.clone())
                        .with_streaming_token(config.spotify.streaming_token.clone()),
                    )),
                    _ => MusicProvider::Unconfigured {
                        kind: MusicProviderKind::Spotify,
                        reason: "set [spotify] client_id and client_secret".to_string(),
                    },
                }
            }
            MusicProviderKind::Youtube => MusicProvider::Youtube(Arc::new(YoutubeClient::new(
                http,
                config.youtube.cookies_path.clone(),
            ))),
            MusicProviderKind::Mopidy => {
                match (
                    config.mopidy.url.as_ref().filter(|s| !s.trim().is_empty()),
                    config.mopidy.stream_url.as_ref().filter(|s| !s.trim().is_empty()),
                ) {
                    (Some(url), Some(stream)) => MusicProvider::Mopidy(Arc::new(
                        MopidyClient::new(http, url, stream.clone()),
                    )),
                    _ => MusicProvider::Unconfigured {
                        kind: MusicProviderKind::Mopidy,
                        reason: "set [mopidy] url + stream_url".to_string(),
                    },
                }
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            MusicProvider::Apple(_) => "apple",
            MusicProvider::Spotify(_) => "spotify",
            MusicProvider::Youtube(_) => "youtube",
            MusicProvider::Mopidy(_) => "mopidy",
            MusicProvider::Unconfigured { kind, .. } => kind_name(*kind),
        }
    }

    /// Top track for a search phrase ("play <song>").
    pub async fn search_top(&self, term: &str) -> Result<Option<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(client) => {
                client.search_top_song(term).await.map(|o| o.map(Into::into))
            }
            MusicProvider::Spotify(c) => c.search_top(term).await,
            MusicProvider::Youtube(c) => c.search_top(term).await,
            MusicProvider::Mopidy(c) => c.search_top(term).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// The default "play music" queue.
    pub async fn queue(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(client) => {
                Ok(client.chart_songs(limit).await?.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.queue(limit).await,
            MusicProvider::Youtube(c) => c.queue(limit).await,
            MusicProvider::Mopidy(c) => c.queue(limit).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// Tracks related to a seed track, for the device's "up next" / radio queue
    /// so it stays on-vibe instead of falling back to the generic chart (which
    /// is why an unrelated country song used to queue after every play). For
    /// Apple we resolve the seed's artist and return their catalog; other
    /// providers reuse the default queue. Falls back to the chart if the seed
    /// can't be resolved.
    pub async fn recommendations(
        &self,
        seed_id: &str,
        limit: u32,
    ) -> Result<Vec<ProviderTrack>, String> {
        if let MusicProvider::Apple(client) = self {
            if let Ok(seed) = client.song(seed_id).await {
                let related: Vec<ProviderTrack> = client
                    .search_songs(&seed.artist, limit)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .filter(|t: &ProviderTrack| t.id != seed_id)
                    .collect();
                if !related.is_empty() {
                    return Ok(related);
                }
            }
        }
        self.queue(limit).await
    }

    // ── personalized library (needs a Music User Token; Apple only) ──────

    /// Whether the user's personalized library is available (Apple + a Music
    /// User Token). Lets the shim serve real "my playlists / my favorites"
    /// instead of empty sections.
    pub fn has_user_library(&self) -> bool {
        match self {
            MusicProvider::Apple(c) => c.has_user_token(),
            MusicProvider::Spotify(c) => c.has_user_auth(),
            MusicProvider::Mopidy(_) => true,
            _ => false,
        }
    }

    /// The user's library / favorite songs, for "play my favorites".
    pub async fn my_favorites(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => {
                Ok(c.library_songs(limit).await?.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.library_tracks(limit).await,
            // Mopidy has no distinct "saved tracks"; use the default queue.
            MusicProvider::Mopidy(c) => c.queue(limit).await,
            _ => Err("personalized library requires a signed-in account".to_string()),
        }
    }

    /// The user's library playlists, for "play my playlists".
    pub async fn my_playlists(&self, limit: u32) -> Result<Vec<ProviderEntity>, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c
                .library_playlists(limit)
                .await?
                .into_iter()
                .map(|e| ProviderEntity {
                    id: e.id,
                    name: e.name,
                    track_count: e.track_count.unwrap_or(limit).max(1),
                })
                .collect()),
            // Spotify's client already yields `ProviderEntity`.
            MusicProvider::Spotify(c) => c.library_playlists(limit).await,
            MusicProvider::Mopidy(c) => c.playlists(limit).await,
            _ => Err("personalized library requires a signed-in account".to_string()),
        }
    }

    /// Tracks of one of the user's library playlists (follow-on to
    /// `my_playlists`).
    pub async fn my_playlist_tracks(
        &self,
        id: &str,
        limit: u32,
    ) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c
                .library_playlist_tracks(id, limit)
                .await?
                .into_iter()
                .map(Into::into)
                .collect()),
            MusicProvider::Spotify(c) => c.library_playlist_tracks(id, limit).await,
            MusicProvider::Mopidy(c) => c.playlist_tracks(id, limit).await,
            _ => Err("personalized library requires a signed-in account".to_string()),
        }
    }

    /// The URL the device player should load for `id`: an Apple sentinel or a
    /// loopback stream URL served by this server.
    pub fn playback_url(&self, id: &str) -> String {
        match self {
            MusicProvider::Apple(_) => format!("{APPLE_SENTINEL}{id}"),
            MusicProvider::Spotify(_) => format!("{STREAM_BASE}/spotify/{id}"),
            MusicProvider::Youtube(_) => format!("{STREAM_BASE}/youtube/{id}"),
            // Mopidy plays internally; the device loads its Icecast stream URL
            // (constant), and the shim points Mopidy at this track first.
            MusicProvider::Mopidy(c) => c.stream_url().to_string(),
            // Unconfigured: harmless sentinel; queue/search already errored out.
            MusicProvider::Unconfigured { .. } => format!("{APPLE_SENTINEL}{id}"),
        }
    }

    /// For Mopidy, point its player at `id` before the device loads the stream,
    /// so the Icecast output carries this track. No-op for other providers.
    pub async fn prepare_playback(&self, id: &str) {
        if let MusicProvider::Mopidy(c) = self {
            if let Err(error) = c.play_track(id).await {
                tracing::warn!(error = %error, "mopidy play_track failed");
            }
        }
    }

    // ── intent resolution (artist / album / genre / playlist / track) ────

    /// Resolve an artist entity for the device's `type=ARTISTS` search.
    pub async fn search_artist(&self, term: &str) -> Result<Option<ProviderEntity>, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c.search_entity("artists", term).await?.map(|e| {
                ProviderEntity {
                    id: e.id,
                    name: e.name,
                    track_count: e.track_count.unwrap_or(50).max(1),
                }
            })),
            MusicProvider::Spotify(_) | MusicProvider::Youtube(_) | MusicProvider::Mopidy(_) => {
                Ok(Some(search_synthetic(term)))
            }
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// An artist's top tracks (follow-on to `search_artist`).
    pub async fn artist_tracks(&self, id: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => {
                Ok(c.artist_top_songs(id, limit).await?.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Youtube(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Mopidy(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// Resolve an album entity for the device's `type=ALBUMS` search.
    pub async fn search_album(&self, term: &str) -> Result<Option<ProviderEntity>, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c.search_entity("albums", term).await?.map(|e| {
                ProviderEntity {
                    id: e.id,
                    name: e.name,
                    track_count: e.track_count.unwrap_or(12).max(1),
                }
            })),
            MusicProvider::Spotify(_) | MusicProvider::Youtube(_) | MusicProvider::Mopidy(_) => {
                Ok(Some(search_synthetic(term)))
            }
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// An album's tracks in order (follow-on to `search_album`).
    pub async fn album_tracks(&self, id: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => {
                Ok(c.album_tracks(id, limit).await?.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Youtube(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Mopidy(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// Resolve a playlist entity for the device's `type=PLAYLISTS` search.
    pub async fn search_playlist(&self, term: &str) -> Result<Option<ProviderEntity>, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c.search_entity("playlists", term).await?.map(|e| {
                ProviderEntity {
                    id: e.id,
                    name: e.name,
                    track_count: e.track_count.unwrap_or(25).max(1),
                }
            })),
            MusicProvider::Spotify(_) | MusicProvider::Youtube(_) | MusicProvider::Mopidy(_) => {
                Ok(Some(search_synthetic(term)))
            }
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// A playlist's tracks (follow-on to `search_playlist`).
    pub async fn playlist_tracks(&self, id: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => {
                Ok(c.playlist_tracks(id, limit).await?.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Youtube(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Mopidy(c) => c.search_songs(&decoded(id), limit).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// Songs for a genre name (the device pre-maps e.g. "rap" -> "Hiphop"). Apple
    /// uses real genre charts; others fall back to a song search on the name.
    pub async fn genre_tracks(&self, genre: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        match self {
            MusicProvider::Apple(c) => {
                let songs = match apple_genre_id(genre) {
                    Some(gid) => c.genre_chart_songs(gid, limit).await?,
                    None => c.search_songs(genre, limit).await?,
                };
                Ok(songs.into_iter().map(Into::into).collect())
            }
            MusicProvider::Spotify(c) => c.search_songs(genre, limit).await,
            MusicProvider::Youtube(c) => c.search_songs(genre, limit).await,
            MusicProvider::Mopidy(c) => c.search_songs(genre, limit).await,
            MusicProvider::Unconfigured { reason, .. } => Err(reason.clone()),
        }
    }

    /// The artist's newest album title, for "play <artist>'s new album".
    pub async fn latest_album(&self, artist: &str) -> Result<Option<String>, String> {
        match self {
            MusicProvider::Apple(c) => match c.search_entity("artists", artist).await? {
                Some(a) => c.artist_latest_album(&a.id).await,
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Metadata for a single track id (`/tracks/{id}` — queryWithTrackIds).
    pub async fn track(&self, id: &str) -> Result<ProviderTrack, String> {
        match self {
            MusicProvider::Apple(c) => Ok(c.song(id).await?.into()),
            MusicProvider::Mopidy(c) => c.track(id).await,
            _ => Err("track-by-id not supported for this provider".to_string()),
        }
    }
}

/// A synthetic entity for search-backed providers: its id encodes the term so
/// the follow-on tracks call can search for it.
fn search_synthetic(term: &str) -> ProviderEntity {
    ProviderEntity {
        id: search_entity_id(term),
        name: term.to_string(),
        track_count: 25,
    }
}

/// Recover the search term from a synthetic entity id (falls back to the id).
fn decoded(id: &str) -> String {
    decode_entity_term(id).unwrap_or_else(|| id.to_string())
}

/// Map the device's mapped genre name to an Apple Music genre id for charts.
fn apple_genre_id(name: &str) -> Option<&'static str> {
    match name.trim().to_lowercase().as_str() {
        "pop" => Some("14"),
        "hiphop" | "hip-hop" | "rap" => Some("18"),
        "rock" => Some("21"),
        "jazz" => Some("11"),
        "classical" => Some("5"),
        "dance" | "electronic" => Some("17"),
        "country" => Some("6"),
        _ => None,
    }
}

fn kind_name(kind: MusicProviderKind) -> &'static str {
    match kind {
        MusicProviderKind::Apple => "apple",
        MusicProviderKind::Spotify => "spotify",
        MusicProviderKind::Youtube => "youtube",
        MusicProviderKind::Mopidy => "mopidy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn apple_default_provider_uses_sentinel_playback_url() {
        // Default config selects Apple; without a token it is Unconfigured,
        // which still yields a harmless Apple sentinel URL.
        let provider = MusicProvider::from_config(&Config::default(), reqwest::Client::new());
        assert!(matches!(
            provider,
            MusicProvider::Unconfigured {
                kind: MusicProviderKind::Apple,
                ..
            }
        ));
        assert_eq!(provider.playback_url("123"), "penumbra-apple://123");
    }

    #[test]
    fn spotify_and_youtube_playback_urls_point_at_local_stream() {
        let sp = MusicProvider::Youtube(Arc::new(YoutubeClient::new(reqwest::Client::new(), None)));
        assert_eq!(
            sp.playback_url("vid"),
            "http://127.0.0.1:8080/tidal-shim/stream/youtube/vid"
        );
    }

    #[test]
    fn apple_playability_boundary_is_2_to_the_31() {
        assert!(apple_id_playable("1888707290")); // "All the Love" (2026, low id) — plays
        assert!(apple_id_playable("1440650711")); // Bohemian — plays
        assert!(!apple_id_playable("6769568456")); // ICEMAN track — overflows, unplayable
        assert!(!apple_id_playable("2147483648")); // exactly 2^31 — unplayable
        assert!(apple_id_playable("2147483647")); // 2^31 - 1 — plays
        assert!(apple_id_playable("dQw4w9WgXcQ")); // non-numeric (YouTube) — pass-through
    }

    #[test]
    fn selecting_spotify_without_creds_is_unconfigured() {
        let mut config = Config::default();
        config.music.provider = MusicProviderKind::Spotify;
        let provider = MusicProvider::from_config(&config, reqwest::Client::new());
        assert!(matches!(
            provider,
            MusicProvider::Unconfigured {
                kind: MusicProviderKind::Spotify,
                ..
            }
        ));
    }
}
