//! Mopidy backend: delegate search / library / playback to a user-run
//! [Mopidy](https://docs.mopidy.com) server over its HTTP JSON-RPC API.
//!
//! This is the "bring your own providers" path: the user installs whatever
//! Mopidy backend extensions they want (Spotify, YouTube, TIDAL, SoundCloud,
//! local, Bandcamp, …) and points us at their Mopidy instance — we don't
//! implement each provider ourselves.
//!
//! Audio: Mopidy has no per-track HTTP URL (it plays internally via GStreamer),
//! so the user configures Mopidy to stream its output to an Icecast server
//! (`lamemp3enc ! shout2send`). We drive Mopidy's tracklist/playback over
//! JSON-RPC and hand the device that single Icecast MP3 URL to play — the same
//! server-decode model as the other non-Apple providers.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::music::{ProviderEntity, ProviderTrack};

/// Client for a Mopidy server's JSON-RPC endpoint (`{base}/mopidy/rpc`) plus the
/// Icecast stream URL its audio output is sent to.
pub struct MopidyClient {
    http: reqwest::Client,
    rpc_url: String,
    stream_url: String,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize, Default)]
struct MopidyTrack {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<MopidyArtist>,
    #[serde(default)]
    album: Option<MopidyAlbum>,
    /// Track length in milliseconds.
    #[serde(default)]
    length: Option<u64>,
}

#[derive(Deserialize, Default)]
struct MopidyArtist {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct MopidyAlbum {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct MopidyRef {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
}

impl MopidyTrack {
    fn into_provider(self) -> ProviderTrack {
        ProviderTrack {
            id: encode_uri(&self.uri),
            title: self.name,
            artist: self.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            album: self.album.map(|a| a.name).unwrap_or_default(),
            duration_ms: self.length.unwrap_or(0),
        }
    }
}

/// Mopidy track URIs (`spotify:track:…`, `yt:…`) contain `:` etc., so carry them
/// as URL-safe base64 in the shim's track/entity id, decoding on the way back.
pub fn encode_uri(uri: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(uri)
}

fn decode_uri(id: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(id).ok()?;
    String::from_utf8(bytes).ok()
}

impl MopidyClient {
    pub fn new(http: reqwest::Client, base_url: &str, stream_url: String) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            http,
            rpc_url: format!("{base}/mopidy/rpc"),
            stream_url,
        }
    }

    /// The Icecast MP3 stream URL the device plays for any Mopidy track.
    pub fn stream_url(&self) -> &str {
        &self.stream_url
    }

    /// Issue a JSON-RPC call and return the `result` value.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("mopidy request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("mopidy {} {}", method, resp.status()));
        }
        let parsed: RpcResponse = resp
            .json()
            .await
            .map_err(|e| format!("mopidy decode failed: {e}"))?;
        if let Some(error) = parsed.error {
            return Err(format!("mopidy {method} error: {error}"));
        }
        Ok(parsed.result)
    }

    /// Search tracks across every configured Mopidy backend.
    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let results = self
            .rpc("core.library.search", json!({ "query": { "any": [term] } }))
            .await?;
        // `search` returns one SearchResult per backend; flatten their tracks.
        let mut tracks: Vec<ProviderTrack> = results
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("tracks").and_then(|t| t.as_array()))
            .flatten()
            .filter_map(|t| serde_json::from_value::<MopidyTrack>(t.clone()).ok())
            .map(MopidyTrack::into_provider)
            .collect();
        tracks.truncate(limit as usize);
        Ok(tracks)
    }

    pub async fn search_top(&self, term: &str) -> Result<Option<ProviderTrack>, String> {
        Ok(self.search_songs(term, 1).await?.into_iter().next())
    }

    /// The user's Mopidy playlists as entities.
    pub async fn playlists(&self, limit: u32) -> Result<Vec<ProviderEntity>, String> {
        let refs = self.rpc("core.playlists.as_list", json!({})).await?;
        let mut out: Vec<ProviderEntity> = refs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| serde_json::from_value::<MopidyRef>(r.clone()).ok())
            .map(|r| ProviderEntity {
                id: encode_uri(&r.uri),
                name: r.name,
                track_count: 1,
            })
            .collect();
        out.truncate(limit as usize);
        Ok(out)
    }

    /// Tracks of a Mopidy playlist (id = encoded playlist uri).
    pub async fn playlist_tracks(&self, id: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let uri = decode_uri(id).ok_or_else(|| "bad mopidy playlist id".to_string())?;
        let playlist = self.rpc("core.playlists.lookup", json!({ "uri": uri })).await?;
        let mut tracks: Vec<ProviderTrack> = playlist
            .get("tracks")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
            .filter_map(|t| serde_json::from_value::<MopidyTrack>(t.clone()).ok())
            .map(MopidyTrack::into_provider)
            .collect();
        tracks.truncate(limit as usize);
        Ok(tracks)
    }

    /// Resolve a single track (id = encoded track uri) for now-playing metadata.
    pub async fn track(&self, id: &str) -> Result<ProviderTrack, String> {
        let uri = decode_uri(id).ok_or_else(|| "bad mopidy track id".to_string())?;
        let looked = self
            .rpc("core.library.lookup", json!({ "uris": [uri.clone()] }))
            .await?;
        // Returns { uri: [Track, ...] }.
        looked
            .get(&uri)
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
            .and_then(|t| serde_json::from_value::<MopidyTrack>(t.clone()).ok())
            .map(MopidyTrack::into_provider)
            .ok_or_else(|| format!("mopidy track not found: {uri}"))
    }

    /// Point Mopidy's player at a track (id = encoded uri) and start it, so the
    /// Icecast stream carries this track. Called from the shim's playbackinfo.
    pub async fn play_track(&self, id: &str) -> Result<(), String> {
        let uri = decode_uri(id).ok_or_else(|| "bad mopidy track id".to_string())?;
        self.rpc("core.tracklist.clear", json!({})).await?;
        self.rpc("core.tracklist.add", json!({ "uris": [uri] })).await?;
        self.rpc("core.playback.play", json!({})).await?;
        Ok(())
    }

    /// Transport proxied to Mopidy (for voice control_music actions).
    pub async fn transport(&self, action: &str) -> Result<(), String> {
        let method = match action {
            "pause" => "core.playback.pause",
            "resume" => "core.playback.resume",
            "next" => "core.playback.next",
            "previous" => "core.playback.previous",
            "stop" => "core.playback.stop",
            _ => return Err(format!("unsupported mopidy transport: {action}")),
        };
        self.rpc(method, json!({})).await.map(|_| ())
    }

    /// A "play music" queue: the first playlist's tracks, else a broad search.
    pub async fn queue(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        if let Ok(playlists) = self.playlists(1).await {
            if let Some(first) = playlists.first() {
                let tracks = self.playlist_tracks(&first.id, limit).await?;
                if !tracks.is_empty() {
                    return Ok(tracks);
                }
            }
        }
        self.search_songs("top hits", limit).await
    }
}
