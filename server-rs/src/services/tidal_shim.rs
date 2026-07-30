//! Minimal Tidal-API-shaped shim (PoC).
//!
//! The music experience's Tidal REST client is redirected here by the
//! `EndpointTypeBypass` hook (host `https://api.tidal.com` -> this server's
//! `/tidal-shim`), and `TidalAuthBypass` feeds it a stub token so it actually
//! issues requests. This shim answers just enough of the "play featured music"
//! chain to drive Android MediaPlayer:
//!
//!   GET /featured/recommended/playlists  -> one playlist (uuid `poc-playlist`)
//!   GET /playlists/{uuid}/items          -> one track  (id `1`)
//!   GET /tracks/{id}/playbackinfopostpaywall -> PlaybackTrackInfo whose base64
//!       `manifest` embeds a fixed test stream URL.
//!
//! Every track resolves to the same test audio; this is not a real provider.
//! JSON keys mirror the app's Gson models (see the tidal-redirect-spike memory);
//! Gson ignores missing fields, so objects are intentionally minimal.

use axum::extract::Path;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{info, warn};

/// Test stream every track resolves to in this PoC. Served BY THIS SHIM over
/// plain-HTTP loopback (not an external host) so playback has no dependency on
/// TLS/cert validation (the device clock is far in the future and Android's
/// HttpURLConnection rejects such certs), DNS, or internet reachability.
const TEST_STREAM_URL: &str = "http://127.0.0.1:8080/tidal-shim/audio/tone.wav";
const MANIFEST_MIME: &str = "audio/wav";
const MANIFEST_CODECS: &str = "1";

const PLAYLIST_UUID: &str = "poc-playlist";
const TRACK_ID: &str = "1";

/// Generated tone parameters.
const TONE_SAMPLE_RATE: u32 = 44100;
const TONE_SECONDS: u32 = 20;
const TONE_HZ: f64 = 440.0;

pub fn router() -> Router {
    Router::new()
        .route(
            "/tidal-shim/v1/featured/recommended/playlists",
            get(featured_playlists),
        )
        .route("/tidal-shim/v1/playlists/{uuid}/items", get(playlist_items))
        .route(
            "/tidal-shim/v1/tracks/{id}/recommendations",
            get(track_recommendations),
        )
        // Trailing slash matches Method.SEARCH ("/search/top-hits/").
        .route("/tidal-shim/v1/search/top-hits/", get(search_top_hits))
        .route(
            "/tidal-shim/v1/tracks/{id}/playbackinfopostpaywall",
            get(playback_info),
        )
        // Locally-served test audio (plain HTTP loopback) that playback resolves to.
        .route("/tidal-shim/audio/tone.wav", get(tone_wav))
        // Any other tidal-shim path: return a JSON object (not plain text) so the
        // app's Gson `TidalErrorResponse.fromResponse` doesn't crash on parse.
        .route("/tidal-shim/{*rest}", get(unmatched).post(unmatched))
}

/// One featured playlist pointing at our single PoC playlist.
///
/// The client parses the body directly into `SearchResultSection$PlaylistSection`
/// (`TidalFeaturedPlaylistsResponse.fromResponse` → `gson.fromJson(body,
/// PlaylistSection.class)`), so the body IS the section — no wrapper object.
/// The playlists list is annotated `@SerializedName("items")`, so the JSON key
/// is `items`, not `playlists`.
async fn featured_playlists() -> impl IntoResponse {
    info!(">>> tidal-shim featured/recommended/playlists");
    Json(json!({
        "items": [ playlist_json(PLAYLIST_UUID) ],
        "limit": 15,
        "offset": 0,
        "totalNumberOfItems": 1
    }))
}

/// The PoC playlist's single track.
///
/// Body is parsed directly into `PlaylistTracks` (no wrapper). Each element is a
/// `TrackItem` decoded by the custom `TidalTrackItemDeserializer`, which reads
/// `type` and the model from JSON key `item` (NOT the Java field name `model`);
/// for `type=="track"` the `item` is parsed as `Track`.
async fn playlist_items(Path(uuid): Path<String>) -> impl IntoResponse {
    info!(uuid = %uuid, ">>> tidal-shim playlists/{{uuid}}/items");
    Json(json!({
        "items": [ { "type": "track", "item": track_json(TRACK_ID) } ],
        "limit": 1,
        "offset": 0,
        "totalNumberOfItems": 1
    }))
}

/// Track recommendations (played when a queue runs dry / "play more like this").
///
/// Body parses into `TrackRecommendations` directly; the query maps
/// `getItems()` -> `item.getTrack()`. Element is `{track, sources}`.
async fn track_recommendations(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/recommendations");
    Json(json!({
        "items": [ { "track": track_json(TRACK_ID), "sources": ["SUGGESTED_TRACKS"] } ],
        "limit": 1,
        "offset": 0,
        "totalNumberOfItems": 1
    }))
}

/// Search ("play <song>"). Body parses into `SearchResult` directly. The
/// track-name query reads `topHits()` (must be non-null) and
/// `tracks().tracks()` (TrackSection.items via @SerializedName("items")), and
/// plays the first result. Other sections are returned non-null/empty so search
/// queries for other content types degrade to "not found" rather than NPE.
async fn search_top_hits() -> impl IntoResponse {
    info!(">>> tidal-shim search/top-hits");
    let empty_section = json!({ "items": [], "limit": 0, "offset": 0, "totalNumberOfItems": 0 });
    Json(json!({
        "topHits": [],
        "genres": [],
        "tracks": {
            "items": [ track_json(TRACK_ID) ],
            "limit": 1, "offset": 0, "totalNumberOfItems": 1
        },
        "albums": empty_section.clone(),
        "artists": empty_section.clone(),
        "playlists": empty_section.clone(),
        "videos": empty_section
    }))
}

/// Playback info: a `PlaybackTrackInfo` whose base64 `manifest` (BTS type)
/// carries the test stream URL for MediaPlayer.
async fn playback_info(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim playbackinfopostpaywall");

    let manifest_json = json!({
        "mimeType": MANIFEST_MIME,
        "codecs": MANIFEST_CODECS,
        "encryptionType": "NONE",
        "urls": [ TEST_STREAM_URL ],
    });
    let manifest = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&manifest_json).unwrap_or_default());

    Json(json!({
        "trackId": id,
        "assetPresentation": "FULL",
        "audioMode": "STEREO",
        "audioQuality": "HIGH",
        "manifestMimeType": "application/vnd.tidal.bts",
        "manifestHash": "poc",
        "manifest": manifest,
        "albumPeakAmplitude": null,
        "albumReplayGain": null,
        "trackPeakAmplitude": null,
        "trackReplayGain": null,
    }))
}

/// Serves a generated 16-bit PCM mono WAV sine tone over plain HTTP loopback.
/// Proves the redirect→shim→ExoPlayer audio path end-to-end without any external
/// stream (no TLS, DNS, or internet). ExoPlayer's WavExtractor handles it.
async fn tone_wav() -> impl IntoResponse {
    info!(">>> tidal-shim audio/tone.wav");
    (
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::ACCEPT_RANGES, "bytes"),
        ],
        generate_tone_wav(),
    )
}

fn generate_tone_wav() -> Vec<u8> {
    let sample_rate = TONE_SAMPLE_RATE;
    let num_samples = sample_rate * TONE_SECONDS;
    let bytes_per_sample = 2u32; // 16-bit mono
    let data_len = num_samples * bytes_per_sample;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt chunk (PCM)
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = 1
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * bytes_per_sample).to_le_bytes()); // byte rate
    buf.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    let two_pi_f_over_sr = 2.0 * std::f64::consts::PI * TONE_HZ / sample_rate as f64;
    for n in 0..num_samples {
        let sample = (two_pi_f_over_sr * n as f64).sin();
        let amplitude = (sample * 0.3 * i16::MAX as f64) as i16; // 30% volume
        buf.extend_from_slice(&amplitude.to_le_bytes());
    }
    buf
}

fn playlist_json(uuid: &str) -> Value {
    json!({
        "uuid": uuid,
        "title": "Penumbra Mix",
        "description": "Local shim playlist",
        "numberOfTracks": 1,
        "numberOfVideos": 0,
        "duration": 180,
        "publicPlaylist": true,
        "type": "EDITORIAL",
        "url": "",
        "image": "",
        "squareImage": "",
        "popularity": 0,
        "created": "2020-01-01T00:00:00.000+0000",
        "lastUpdated": "2020-01-01T00:00:00.000+0000",
        "lastItemAddedAt": "2020-01-01T00:00:00.000+0000",
        "promotedArtists": [],
        "creator": { "id": 0, "name": "Penumbra" }
    })
}

fn track_json(id: &str) -> Value {
    json!({
        "id": id,
        "title": "SoundHelix Song 1",
        "duration": 180,
        "trackNumber": 1,
        "volumeNumber": 1,
        "popularity": 0,
        "explicit": false,
        "allowStreaming": true,
        "streamReady": true,
        "premiumStreamingOnly": false,
        "editable": false,
        "audioQuality": "HIGH",
        "audioModes": [ "STEREO" ],
        "url": "",
        "isrc": "",
        "copyright": "",
        "peak": null,
        "replayGain": null,
        "version": null,
        "artists": [ { "id": 0, "name": "Penumbra", "type": "MAIN" } ],
        "album": { "id": 0, "title": "Penumbra Shim", "cover": null, "videoCover": null, "url": "" }
    })
}

async fn unmatched(request: axum::extract::Request) -> impl IntoResponse {
    warn!(method = %request.method(), path = %request.uri(), "tidal-shim: unimplemented endpoint");
    // JSON object so Gson-based error parsing on the client does not crash.
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({ "status": 404, "subStatus": 404, "userMessage": "tidal-shim: not implemented" })),
    )
}
