//! Minimal Tidal-API-shaped mock served at `/tidal-shim`.
//!
//! The music experience's Tidal REST client is redirected here by the
//! `EndpointTypeBypass` hook (host `api.tidal.com` -> this server) and fed a
//! stub token by `TidalAuthBypass`, so it issues requests. This mock answers
//! just enough of the "play music" chain to drive the on-device player with a
//! local test tone — no external provider. JSON keys mirror the app's Gson
//! models (Gson ignores missing fields, so objects are intentionally minimal);
//! real providers plug in behind these same routes later.

use std::collections::HashMap;

use axum::extract::{Path, Query, Request};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{info, warn};

/// Where the redirected client reaches this server (see `EndpointTypeBypass`).
const SHIM_BASE: &str = "http://127.0.0.1:8080/tidal-shim";
const PLAYLIST_UUID: &str = "poc-playlist";
const QUEUE_SIZE: usize = 6;

// Generated test-tone parameters (44.1 kHz 16-bit mono sine).
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
        .route("/tidal-shim/v1/tracks/{id}/radio", get(track_radio))
        .route("/tidal-shim/v1/tracks/{id}", get(single_track))
        // Trailing slash matches the client's Method.SEARCH ("/search/top-hits/").
        .route("/tidal-shim/v1/search/top-hits/", get(search_top_hits))
        .route(
            "/tidal-shim/v1/tracks/{id}/playbackinfopostpaywall",
            get(playback_info),
        )
        .route("/tidal-shim/audio/tone.wav", get(tone_wav))
        // Any other path: a JSON object so the client's Gson error parsing does
        // not crash on a non-object body.
        .route("/tidal-shim/{*rest}", get(unmatched).post(unmatched))
}

/// One featured playlist. The body parses directly into `PlaylistSection`, whose
/// list field is `@SerializedName("items")`.
async fn featured_playlists() -> impl IntoResponse {
    info!(">>> tidal-shim featured/recommended/playlists");
    Json(json!({
        "items": [ playlist_json() ],
        "limit": 1,
        "offset": 0,
        "totalNumberOfItems": 1
    }))
}

async fn playlist_items(Path(uuid): Path<String>) -> impl IntoResponse {
    info!(uuid = %uuid, ">>> tidal-shim playlists/{{uuid}}/items");
    track_item_wrapper(mock_queue())
}

/// A single track (`/tracks/{id}` — body is a bare `Track`).
async fn single_track(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}");
    Json(mock_track(&id))
}

async fn track_radio(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/radio");
    wrapper(mock_queue())
}

/// Element shape is `{track, sources}` (played when a queue runs dry).
async fn track_recommendations(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/recommendations");
    let items: Vec<Value> = mock_queue()
        .into_iter()
        .map(|t| json!({ "track": t, "sources": ["SUGGESTED_TRACKS"] }))
        .collect();
    let n = items.len();
    Json(json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n }))
}

/// Search ("play <anything>"). The track-name query reads `topHits()` and
/// `tracks().items`; every section must be present or the client NPEs.
async fn search_top_hits(Query(params): Query<HashMap<String, String>>) -> impl IntoResponse {
    let term = params
        .get("query")
        .or_else(|| params.get("term"))
        .cloned()
        .unwrap_or_default();
    info!(term = %term, ">>> tidal-shim search/top-hits");
    let tracks = mock_queue();
    let top = tracks.first().cloned().unwrap_or_else(|| mock_track("mock-1"));
    Json(json!({
        "topHits": [ { "type": "TRACKS", "value": top } ],
        "genres": [],
        "tracks": section(tracks),
        "albums": empty_section(),
        "artists": empty_section(),
        "playlists": empty_section(),
        "videos": empty_section()
    }))
}

/// Playback info: a `PlaybackTrackInfo` whose base64 BTS `manifest` points every
/// track at the local test tone.
async fn playback_info(Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim playbackinfopostpaywall (mock tone)");
    let manifest_json = json!({
        "mimeType": "audio/wav",
        "codecs": "1",
        "encryptionType": "NONE",
        "urls": [ format!("{SHIM_BASE}/audio/tone.wav") ],
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
        "trackReplayGain": null
    }))
}

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

async fn unmatched(request: Request) -> impl IntoResponse {
    warn!(method = %request.method(), path = %request.uri(), "tidal-shim: unimplemented endpoint");
    Json(json!({}))
}

// ── JSON helpers (keys mirror the app's Gson models) ─────────────────────

fn mock_queue() -> Vec<Value> {
    (1..=QUEUE_SIZE)
        .map(|i| mock_track(&format!("mock-{i}")))
        .collect()
}

/// Every id plays the same tone; the title just distinguishes queue entries so
/// next/previous are visibly different.
fn mock_track(id: &str) -> Value {
    track_json(id, &format!("Penumbra Test Tone ({id})"), "Penumbra", "Shim Mock", 20)
}

fn track_json(id: &str, title: &str, artist: &str, album: &str, duration_secs: u64) -> Value {
    json!({
        "id": id,
        "title": title,
        "duration": duration_secs,
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
        "artists": [ { "id": 0, "name": artist, "type": "MAIN" } ],
        "album": { "id": 0, "title": album, "cover": null, "videoCover": null, "url": "" }
    })
}

fn playlist_json() -> Value {
    json!({
        "uuid": PLAYLIST_UUID,
        "title": "Penumbra Mix",
        "description": "Local shim playlist",
        "numberOfTracks": QUEUE_SIZE,
        "numberOfVideos": 0,
        "duration": QUEUE_SIZE * 210,
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

/// The recurring list container `{items, limit, offset, totalNumberOfItems}`.
fn section(items: Vec<Value>) -> Value {
    let n = items.len();
    json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n })
}

fn empty_section() -> Value {
    section(vec![])
}

fn wrapper(items: Vec<Value>) -> Json<Value> {
    Json(section(items))
}

/// Album/playlist track lists deliver each track as `{type:"track", item:<Track>}`.
fn track_item_wrapper(tracks: Vec<Value>) -> Json<Value> {
    let items: Vec<Value> = tracks
        .into_iter()
        .map(|t| json!({ "type": "track", "item": t }))
        .collect();
    Json(section(items))
}

/// A 20-second 440 Hz sine as a PCM WAV, used as the mock's audio for every track.
fn generate_tone_wav() -> Vec<u8> {
    let sample_rate = TONE_SAMPLE_RATE;
    let num_samples = sample_rate * TONE_SECONDS;
    let bytes_per_sample = 2u32; // 16-bit mono
    let data_len = num_samples * bytes_per_sample;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = 1
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * bytes_per_sample).to_le_bytes()); // byte rate
    buf.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    let step = 2.0 * std::f64::consts::PI * TONE_HZ / sample_rate as f64;
    for n in 0..num_samples {
        let amplitude = ((step * n as f64).sin() * 0.3 * i16::MAX as f64) as i16; // 30% volume
        buf.extend_from_slice(&amplitude.to_le_bytes());
    }
    buf
}
