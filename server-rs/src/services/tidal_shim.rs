//! Minimal Tidal-API-shaped shim served at `/tidal-shim`, backed by a
//! [`MusicProvider`](crate::music::MusicProvider). The music app's Tidal client
//! is redirected here by `EndpointTypeBypass`; the shim asks the configured
//! provider for tracks and returns them in the app's Tidal wire format. JSON
//! keys mirror the app's Gson models.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::music::{ProviderTrack, SharedProvider};

const PLAYLIST_UUID: &str = "poc-playlist";
const QUEUE_SIZE: usize = 6;

const TONE_SAMPLE_RATE: u32 = 44100;
const TONE_SECONDS: u32 = 20;
const TONE_HZ: f64 = 440.0;

/// The app's Tidal models type an artist id as a number, so a synthetic
/// numeric id stands in for each artist name and this registry maps it back.
/// (A track id is a string in the same models, which is why those pass through
/// untouched.) Populated whenever an artist is emitted, read when the app
/// follows up on one.
#[derive(Clone, Default)]
pub struct ArtistRegistry(Arc<RwLock<HashMap<u64, String>>>);

impl ArtistRegistry {
    /// Stable id for a name (FNV-1a), remembered for the reverse lookup.
    fn register(&self, name: &str) -> u64 {
        let id = artist_id(name);
        if let Ok(mut map) = self.0.write() {
            map.insert(id, name.to_string());
        }
        id
    }

    fn name(&self, id: u64) -> Option<String> {
        self.0.read().ok()?.get(&id).cloned()
    }
}

/// FNV-1a over the lowercased name, squeezed into a comfortably small positive
/// range so it survives any 32/64-bit signed field on the client.
fn artist_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.to_lowercase().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash % 1_000_000_000 + 1
}

#[derive(Clone)]
pub struct ShimState {
    provider: SharedProvider,
    artists: ArtistRegistry,
}

pub fn router(provider: SharedProvider) -> Router {
    let state = ShimState {
        provider,
        artists: ArtistRegistry::default(),
    };
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
        // The client requests "/search/top-hits/" with the trailing slash verbatim.
        .route("/tidal-shim/v1/search/top-hits/", get(search_top_hits))
        .route(
            "/tidal-shim/v1/tracks/{id}/playbackinfopostpaywall",
            get(playback_info),
        )
        // An artist-name request ("play <artist>") resolves the artist first,
        // then pulls a body of work from one of these.
        .route("/tidal-shim/v1/artists/{id}", get(artist))
        .route("/tidal-shim/v1/artists/{id}/toptracks", get(artist_tracks))
        .route("/tidal-shim/v1/artists/{id}/tracks", get(artist_tracks))
        .route("/tidal-shim/v1/artists/{id}/radio", get(artist_tracks))
        .route("/tidal-shim/v1/artists/{id}/mix", get(artist_tracks))
        .route("/tidal-shim/v1/artists/{id}/albums", get(artist_albums))
        .route("/tidal-shim/v1/artists/{id}/videos", get(artist_videos))
        .route("/tidal-shim/v1/artists/{id}/bio", get(artist_bio))
        .route("/tidal-shim/audio/tone.wav", get(tone_wav))
        // Fallback: an object, so the client's Gson error-parsing can't crash on a non-object body.
        .route("/tidal-shim/{*rest}", get(unmatched).post(unmatched))
        .with_state(state)
}

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
    State(state): State<ShimState>,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    let provider = &state.provider;
    info!(uuid = %uuid, provider = provider.name(), ">>> tidal-shim playlists/{{uuid}}/items");
    track_item_wrapper(tracks_json(provider.queue(QUEUE_SIZE).await))
}

async fn single_track(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let provider = &state.provider;
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}");
    Json(track_json_from_provider(&provider.track(&id).await))
}

async fn track_radio(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let provider = &state.provider;
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/radio");
    wrapper(tracks_json(provider.recommendations(&id, QUEUE_SIZE).await))
}

async fn track_recommendations(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let provider = &state.provider;
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/recommendations");
    let items: Vec<Value> = tracks_json(provider.recommendations(&id, QUEUE_SIZE).await)
        .into_iter()
        .map(|t| json!({ "track": t, "sources": ["SUGGESTED_TRACKS"] }))
        .collect();
    let n = items.len();
    Json(json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n }))
}

// Every section below must be present or the client NPEs.
async fn search_top_hits(
    State(state): State<ShimState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let provider = &state.provider;
    let term = params
        .get("query")
        .or_else(|| params.get("term"))
        .cloned()
        .unwrap_or_default();
    info!(term = %term, provider = provider.name(), ">>> tidal-shim search/top-hits");
    // Every result must actually match the term: the app picks an artist out of
    // this response, so padding it with unrelated "recommendations" lets it play
    // something the user never asked for.
    let mut tracks = provider.search_many(&term, QUEUE_SIZE).await;
    if tracks.is_empty() {
        tracks = provider.queue(QUEUE_SIZE).await;
    }
    // The app answers "play <artist>" through TidalArtistNameCollectionQuery,
    // which reads this `artists` section — leaving it empty makes the app throw
    // TidalResultsNotFoundException and never ask for a stream, so the request
    // dies silently after the search.
    // The per-track artist is YouTube's *uploader*, which is frequently a
    // channel rather than the performer — searching "michael jackson" yields
    // uploaders like "Dlo", and the app will happily resolve the artist to one
    // of those and play it. Lead with an artist named for the search term
    // itself, so the name the user said is the one available to match.
    let artist_names = std::iter::once(term.trim().to_string())
        .filter(|term| !term.is_empty())
        .chain(artist_names_from(&tracks))
        .collect::<Vec<_>>();
    let mut seen: Vec<String> = Vec::new();
    let artists: Vec<Value> = artist_names
        .into_iter()
        .filter(|name| {
            let fresh = !seen.iter().any(|s| s.eq_ignore_ascii_case(name));
            if fresh {
                seen.push(name.clone());
            }
            fresh
        })
        .map(|name| artist_json(state.artists.register(&name), &name))
        .collect();

    let tracks = tracks_json(tracks);
    let top = tracks.first().cloned().unwrap_or_else(|| json!({}));
    Json(json!({
        "topHits": [ { "type": "TRACKS", "value": top } ],
        "genres": [],
        "tracks": section(tracks),
        "albums": empty_section(),
        "artists": section(artists),
        "playlists": empty_section(),
        "videos": empty_section()
    }))
}

/// Resolve an artist id back to the name the search registered, falling back to
/// the raw id so a restarted server degrades to "something plays" rather than
/// an error.
async fn artist_name_for(state: &ShimState, id: &str) -> String {
    id.parse::<u64>()
        .ok()
        .and_then(|id| state.artists.name(id))
        .unwrap_or_else(|| id.to_string())
}

async fn artist(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    let name = artist_name_for(&state, &id).await;
    info!(artist_id = %id, %name, ">>> tidal-shim artists/{{id}}");
    Json(artist_json(id.parse().unwrap_or_else(|_| artist_id(&name)), &name))
}

async fn artist_tracks(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let name = artist_name_for(&state, &id).await;
    info!(artist_id = %id, %name, provider = state.provider.name(), ">>> tidal-shim artists/{{id}} tracks");
    let mut tracks = state.provider.search_many(&name, QUEUE_SIZE).await;
    if tracks.is_empty() {
        tracks = state.provider.queue(QUEUE_SIZE).await;
    }
    Json(section(tracks_json(tracks)))
}

async fn artist_albums(Path(id): Path<String>) -> impl IntoResponse {
    info!(artist_id = %id, ">>> tidal-shim artists/{{id}}/albums");
    Json(empty_section())
}

async fn artist_videos(Path(id): Path<String>) -> impl IntoResponse {
    info!(artist_id = %id, ">>> tidal-shim artists/{{id}}/videos");
    Json(empty_section())
}

async fn artist_bio(Path(id): Path<String>) -> impl IntoResponse {
    info!(artist_id = %id, ">>> tidal-shim artists/{{id}}/bio");
    Json(json!({ "source": "", "lastUpdated": "2020-01-01T00:00:00.000+0000", "text": "" }))
}

async fn playback_info(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let provider = &state.provider;
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

async fn tone_wav(headers: HeaderMap) -> impl IntoResponse {
    info!(">>> tidal-shim audio/tone.wav");
    let body = generate_tone_wav();
    let total = body.len();
    let Some(range) = headers.get(header::RANGE).and_then(|value| value.to_str().ok()) else {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .body(axum::body::Body::from(body))
            .unwrap();
    };

    let Some(range) = range.strip_prefix("bytes=")
        .and_then(|value| value.split(',').next())
        .and_then(|value| {
            let (start, end) = value.split_once('-')?;
            let start = start.parse::<usize>().ok()?;
            let end = if end.is_empty() { total.saturating_sub(1) } else { end.parse().ok()? };
            (start <= end && start < total).then_some((start, end.min(total - 1)))
        }) else {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(axum::body::Body::empty())
            .unwrap();
    };

    let (start, end) = range;
    let content_length = end - start + 1;
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
        .header(header::CONTENT_LENGTH, content_length)
        .body(axum::body::Body::from(body[start..=end].to_vec()))
        .unwrap()
}

async fn unmatched(request: Request) -> impl IntoResponse {
    warn!(method = %request.method(), path = %request.uri(), "tidal-shim: unimplemented endpoint");
    Json(json!({}))
}

// ── ProviderTrack -> Tidal JSON ──────────────────────────────────────────

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

/// Distinct artist names from a result set, best match first.
fn artist_names_from(tracks: &[ProviderTrack]) -> Vec<String> {
    let mut seen = Vec::new();
    for track in tracks {
        let name = track.artist.trim();
        if !name.is_empty() && !seen.iter().any(|s: &String| s.eq_ignore_ascii_case(name)) {
            seen.push(name.to_string());
        }
    }
    seen
}

fn artist_json(id: u64, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "artistTypes": [ "ARTIST" ],
        "artistRoles": [ { "categoryId": 1, "category": "Main" } ],
        "picture": null,
        "popularity": 0,
        "url": ""
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

fn track_item_wrapper(tracks: Vec<Value>) -> Json<Value> {
    let items: Vec<Value> = tracks
        .into_iter()
        .map(|t| json!({ "type": "track", "item": t }))
        .collect();
    Json(section(items))
}

fn generate_tone_wav() -> Vec<u8> {
    let sample_rate = TONE_SAMPLE_RATE;
    let num_samples = sample_rate * TONE_SECONDS;
    let bytes_per_sample = 2u32;
    let data_len = num_samples * bytes_per_sample;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * bytes_per_sample).to_le_bytes());
    buf.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    let step = 2.0 * std::f64::consts::PI * TONE_HZ / sample_rate as f64;
    for n in 0..num_samples {
        let amplitude = ((step * n as f64).sin() * 0.3 * i16::MAX as f64) as i16;
        buf.extend_from_slice(&amplitude.to_le_bytes());
    }
    buf
}
