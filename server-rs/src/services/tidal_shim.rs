//! Minimal Tidal-API-shaped shim served at `/tidal-shim`, backed by a
//! [`MusicProvider`](crate::music::MusicProvider).
//!
//! The music experience's Tidal REST client is redirected here by the
//! `EndpointTypeBypass` hook (host `api.tidal.com` -> this server) and fed a
//! stub token by `TidalAuthBypass`, so it issues requests. The shim answers the
//! "play music" chain by asking the configured provider for tracks and
//! translating them into the app's Tidal wire format (JSON keys mirror the
//! app's Gson models; Gson ignores missing fields, so objects are intentionally
//! minimal). The shim never knows which provider it holds.

use std::collections::HashMap;

use axum::extract::{Path, Query, Request, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::music::{ProviderTrack, SharedProvider};

const PLAYLIST_UUID: &str = "poc-playlist";
/// How many tracks to request for a queue, so next/previous have somewhere to go.
const QUEUE_SIZE: usize = 6;

// Generated test-tone parameters (44.1 kHz 16-bit mono sine).
const TONE_SAMPLE_RATE: u32 = 44100;
const TONE_SECONDS: u32 = 20;
const TONE_HZ: f64 = 440.0;

pub fn router(provider: SharedProvider) -> Router {
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
        .with_state(provider)
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

async fn playlist_items(
    State(provider): State<SharedProvider>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    info!(uuid = %uuid, provider = provider.name(), ">>> tidal-shim playlists/{{uuid}}/items");
    track_item_wrapper(tracks_json(provider.queue(QUEUE_SIZE).await))
}

/// A single track (`/tracks/{id}` — body is a bare `Track`).
async fn single_track(
    State(provider): State<SharedProvider>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}");
    Json(track_json_from_provider(&provider.track(&id).await))
}

async fn track_radio(
    State(provider): State<SharedProvider>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/radio");
    wrapper(tracks_json(provider.recommendations(&id, QUEUE_SIZE).await))
}

/// Element shape is `{track, sources}` (played when a queue runs dry).
async fn track_recommendations(
    State(provider): State<SharedProvider>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/recommendations");
    let items: Vec<Value> = tracks_json(provider.recommendations(&id, QUEUE_SIZE).await)
        .into_iter()
        .map(|t| json!({ "track": t, "sources": ["SUGGESTED_TRACKS"] }))
        .collect();
    let n = items.len();
    Json(json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n }))
}

/// Search ("play <song>"). The track-name query reads `topHits()` and
/// `tracks().items`; every section must be present or the client NPEs.
async fn search_top_hits(
    State(provider): State<SharedProvider>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let term = params
        .get("query")
        .or_else(|| params.get("term"))
        .cloned()
        .unwrap_or_default();
    info!(term = %term, provider = provider.name(), ">>> tidal-shim search/top-hits");
    // The requested track plays first, then related tracks so the device has a
    // real queue instead of a single track that dead-ends on "next".
    let tracks = match provider.search_top(&term).await {
        Some(seed) => {
            let mut list = vec![seed.clone()];
            for t in provider.recommendations(&seed.id, QUEUE_SIZE).await {
                if t.id != seed.id {
                    list.push(t);
                }
            }
            list
        }
        None => provider.queue(QUEUE_SIZE).await,
    };
    let tracks = tracks_json(tracks);
    let top = tracks.first().cloned().unwrap_or_else(|| json!({}));
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

/// Playback info: the provider's URL wrapped in a base64 BTS `manifest` the
/// device's player fetches.
async fn playback_info(
    State(provider): State<SharedProvider>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(track_id = %id, provider = provider.name(), ">>> tidal-shim playbackinfopostpaywall");
    let manifest_json = json!({
        "mimeType": "audio/wav",
        "codecs": "1",
        "encryptionType": "NONE",
        "urls": [ provider.playback(&id).await ],
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

// ── ProviderTrack -> Tidal JSON (keys mirror the app's Gson models) ──────

fn tracks_json(tracks: Vec<ProviderTrack>) -> Vec<Value> {
    tracks.iter().map(track_json_from_provider).collect()
}

fn track_json_from_provider(t: &ProviderTrack) -> Value {
    track_json(&t.id, &t.title, &t.artist, &t.album, (t.duration_ms / 1000).max(1))
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

/// A 20-second 440 Hz sine as a PCM WAV — the mock provider's audio, served here
/// because providers do not own HTTP routes.
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
