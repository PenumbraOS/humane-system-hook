//! Minimal Tidal-API-shaped shim, backed by a pluggable music provider.
//!
//! The music experience's Tidal REST client is redirected here by the
//! `EndpointTypeBypass` hook (host `https://api.tidal.com` -> this server's
//! `/tidal-shim`), and `TidalAuthBypass` feeds it a stub token so it actually
//! issues requests. This shim answers just enough of the "play music" chain to
//! drive the on-device player:
//!
//!   GET /featured/recommended/playlists  -> one playlist (uuid `poc-playlist`)
//!   GET /playlists/{uuid}/items          -> the provider's queue
//!   GET /search/top-hits/                -> the provider's top match for a term
//!   GET /tracks/{id}/playbackinfopostpaywall -> PlaybackTrackInfo whose base64
//!       `manifest` embeds the provider's playback URL.
//!
//! The playback URL is provider-specific ([`crate::music`]): Apple returns a
//! `penumbra-apple://<id>` sentinel the MusicKit hook plays on-device; Spotify /
//! YouTube return a loopback stream URL served under `/tidal-shim/stream/...`
//! that the device's ExoPlayer plays directly. JSON keys mirror the app's Gson
//! models (see the tidal-redirect-spike memory); Gson ignores missing fields, so
//! objects are intentionally minimal.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::music::{MusicProvider, NowPlaying, NowPlayingHandle, ProviderEntity, ProviderTrack};

const MANIFEST_MIME: &str = "audio/mp4";
const MANIFEST_CODECS: &str = "1";

const PLAYLIST_UUID: &str = "poc-playlist";

/// Sentinel search terms the personalized-library flows emit as `PlayMusic`
/// track queries. The device's own library actions (favorites, library
/// playlists) need a logged-in Tidal user session (which we bypass), so they
/// no-op; routing through PlayMusic with these markers lets the shim serve the
/// user's real library instead. Any `__penumbra_` term is handled server-side.
pub const FAVORITES_TERM: &str = "__penumbra_favorites__";
/// Play a shuffled mix drawn from the user's own playlists.
pub const MY_PLAYLISTS_TERM: &str = "__penumbra_my_playlists__";
/// Play a specific user playlist by name: `__penumbra_playlist__:<name>`.
pub const PLAYLIST_PREFIX: &str = "__penumbra_playlist__:";
/// True if a search term is one of our personalized-library sentinels.
pub fn is_personal_term(term: &str) -> bool {
    term.starts_with("__penumbra_")
}

/// Fallback track id/metadata used only when a provider lookup fails, so the
/// client never receives an empty/NPE-inducing response.
const FALLBACK_ID: &str = "1440650711";

/// How many real tracks to put in a "play music" queue / recommendations, so
/// Humane has a genuine multi-track queue that next/prev navigate.
const QUEUE_SIZE: u32 = 15;

/// Generated tone parameters (legacy loopback test audio).
const TONE_SAMPLE_RATE: u32 = 44100;
const TONE_SECONDS: u32 = 20;
const TONE_HZ: f64 = 440.0;

/// Router state: the active music provider + shared now-playing state.
#[derive(Clone)]
pub struct ShimState {
    /// Hot-swappable so a Center provider change applies without a restart.
    pub provider: crate::music::SharedProvider,
    pub now_playing: NowPlayingHandle,
}

impl ShimState {
    /// Snapshot the current provider (cheap Arc clone) for the duration of a
    /// request, so a mid-request swap can't change it under us.
    fn provider(&self) -> Arc<MusicProvider> {
        self.provider.load_full()
    }
}

pub fn router(state: ShimState) -> Router {
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
        // Per-intent resolution (artist / album / genre / playlist / trackId).
        .route("/tidal-shim/v1/artists/{id}/toptracks", get(artist_toptracks))
        .route("/tidal-shim/v1/artists/{id}/albums", get(artist_albums))
        .route("/tidal-shim/v1/albums/{id}/items", get(album_items))
        .route("/tidal-shim/v1/genres/{genre}/tracks", get(genre_tracks))
        .route("/tidal-shim/v1/tracks/{id}", get(single_track))
        .route("/tidal-shim/v1/users/{user}/playlists", get(user_playlists))
        .route(
            "/tidal-shim/v1/users/{user}/favorites/tracks",
            get(user_favorites),
        )
        // Trailing slash matches Method.SEARCH ("/search/top-hits/").
        .route("/tidal-shim/v1/search/top-hits/", get(search_top_hits))
        .route(
            "/tidal-shim/v1/tracks/{id}/playbackinfopostpaywall",
            get(playback_info),
        )
        // Server-decoded audio streams (Spotify/YouTube) the device plays directly.
        .route("/tidal-shim/stream/{provider}/{id}", get(stream_audio))
        // Locally-served test audio (plain HTTP loopback) that playback resolves to.
        .route("/tidal-shim/audio/tone.wav", get(tone_wav))
        // Any other tidal-shim path: return a JSON object (not plain text) so the
        // app's Gson `TidalErrorResponse.fromResponse` doesn't crash on parse.
        .route("/tidal-shim/{*rest}", get(unmatched).post(unmatched))
        .with_state(state)
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

/// The playlist's tracks = the provider's queue.
///
/// Body is parsed directly into `PlaylistTracks` (no wrapper). Each element is a
/// `TrackItem` decoded by the custom `TidalTrackItemDeserializer`, which reads
/// `type` and the model from JSON key `item` (NOT the Java field name `model`);
/// for `type=="track"` the `item` is parsed as `Track`.
async fn playlist_items(State(state): State<ShimState>, Path(uuid): Path<String>) -> impl IntoResponse {
    info!(uuid = %uuid, provider = state.provider().name(), ">>> tidal-shim playlists/{{uuid}}/items");
    // The featured/"play music" playlist uses the chart queue; a real playlist
    // uuid (from a playlist-name search) fetches that playlist's tracks.
    let tracks = if uuid == PLAYLIST_UUID {
        queue_tracks(&state).await
    } else {
        // A `p.` id is one of the user's library playlists (needs the user
        // token); `pl.` and everything else is a catalog playlist.
        let resolved = if uuid.starts_with("p.") && state.provider().has_user_library() {
            state.provider().my_playlist_tracks(&uuid, 100).await
        } else {
            state.provider().playlist_tracks(&uuid, 100).await
        };
        let t = keep_playable(&state.provider(), resolved.unwrap_or_default());
        if t.is_empty() {
            queue_tracks(&state).await
        } else {
            t.iter().map(track_json_from_provider).collect()
        }
    };
    track_item_wrapper(tracks)
}

/// An artist's top tracks (`/artists/{id}/toptracks` → plain `Track` items).
async fn artist_toptracks(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    info!(artist = %id, ">>> tidal-shim artists/{{id}}/toptracks");
    let tracks = keep_playable(&state.provider(), state.provider().artist_tracks(&id, 50).await.unwrap_or_default());
    wrapper(tracks.iter().map(track_json_from_provider).collect())
}

/// Artist albums (`/artists/{id}/albums`) — empty so the device falls back to an
/// album-name search (which the shim serves).
async fn artist_albums(Path(id): Path<String>) -> impl IntoResponse {
    info!(artist = %id, ">>> tidal-shim artists/{{id}}/albums (empty)");
    wrapper(vec![])
}

/// An album's tracks in order (`/albums/{id}/items` → `{type,item}` wrappers).
/// A `pl.`-prefixed id is a playlist surfaced as an album (mood/genre requests).
async fn album_items(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    info!(album = %id, ">>> tidal-shim albums/{{id}}/items");
    let raw = if id.starts_with("pl.") {
        state.provider().playlist_tracks(&id, 100).await.unwrap_or_default()
    } else {
        state.provider().album_tracks(&id, 100).await.unwrap_or_default()
    };
    let tracks = keep_playable(&state.provider(), raw);
    track_item_wrapper(tracks.iter().map(track_json_from_provider).collect())
}

/// Songs for a genre (`/genres/{genre}/tracks` → plain `Track` items).
async fn genre_tracks(State(state): State<ShimState>, Path(genre): Path<String>) -> impl IntoResponse {
    info!(genre = %genre, ">>> tidal-shim genres/{{genre}}/tracks");
    let tracks = keep_playable(&state.provider(), state.provider().genre_tracks(&genre, 50).await.unwrap_or_default());
    wrapper(tracks.iter().map(track_json_from_provider).collect())
}

/// A single track by id (`/tracks/{id}` — body is a bare `Track`).
async fn single_track(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}");
    match state.provider().track(&id).await {
        Ok(t) => Json(track_json_from_provider(&t)),
        Err(error) => {
            warn!(track_id = %id, %error, "track lookup failed, using fallback");
            Json(fallback_track())
        }
    }
}

/// Radio for a track (`/tracks/{id}/radio`) — the chart queue as a station.
async fn track_radio(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/radio");
    wrapper(seed_queue_tracks(&state, &id).await)
}

/// User playlists (`/users/{id}/playlists`) — empty so the device falls to a
/// provider playlist search (library access needs a user token we don't have).
async fn user_playlists(
    State(state): State<ShimState>,
    Path(user): Path<String>,
) -> impl IntoResponse {
    // With a Music User Token we serve the account's real library playlists;
    // without one there is no user library to expose, so stay empty (the device
    // then falls back to a provider playlist search).
    if state.provider().has_user_library() {
        match state.provider().my_playlists(50).await {
            Ok(playlists) if !playlists.is_empty() => {
                info!(user = %user, count = playlists.len(), ">>> tidal-shim users/{{id}}/playlists");
                return wrapper(playlists.iter().map(playlist_json_entity).collect());
            }
            Ok(_) => info!(user = %user, ">>> tidal-shim users/{{id}}/playlists (none)"),
            Err(error) => warn!(user = %user, %error, "user playlists failed"),
        }
    } else {
        info!(user = %user, ">>> tidal-shim users/{{id}}/playlists (no user token)");
    }
    wrapper(vec![])
}

/// User favorites (`/users/{id}/favorites/tracks`) — chart queue stand-in (real
/// library favorites need a Music User Token).
async fn user_favorites(State(state): State<ShimState>, Path(user): Path<String>) -> impl IntoResponse {
    info!(user = %user, ">>> tidal-shim users/{{id}}/favorites/tracks");
    // Real library songs when a Music User Token is present; otherwise the chart
    // queue stand-in so "play my favorites" still plays something.
    let tracks = if state.provider().has_user_library() {
        match state.provider().my_favorites(100).await {
            Ok(t) => {
                let t = keep_playable(&state.provider(), t);
                if t.is_empty() {
                    queue_tracks(&state).await
                } else {
                    t.iter().map(track_json_from_provider).collect()
                }
            }
            Err(error) => {
                warn!(user = %user, %error, "user favorites failed, using chart");
                queue_tracks(&state).await
            }
        }
    } else {
        queue_tracks(&state).await
    };
    let items: Vec<Value> = tracks
        .into_iter()
        .map(|t| json!({ "created": "2020-01-01T00:00:00.000+0000", "item": t }))
        .collect();
    let n = items.len();
    Json(json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n }))
}

/// Track recommendations (played when a queue runs dry / "play more like this").
///
/// Body parses into `TrackRecommendations` directly; the query maps
/// `getItems()` -> `item.getTrack()`. Element is `{track, sources}`.
async fn track_recommendations(
    State(state): State<ShimState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(track_id = %id, ">>> tidal-shim tracks/{{id}}/recommendations");
    let tracks = seed_queue_tracks(&state, &id).await;
    let n = tracks.len();
    let items: Vec<Value> = tracks
        .into_iter()
        .map(|t| json!({ "track": t, "sources": ["SUGGESTED_TRACKS"] }))
        .collect();
    Json(json!({
        "items": items,
        "limit": n,
        "offset": 0,
        "totalNumberOfItems": n
    }))
}

/// Search ("play <song>"). Body parses into `SearchResult` directly. The
/// track-name query reads `topHits()` (must be non-null) and
/// `tracks().tracks()` (TrackSection.items via @SerializedName("items")), and
/// plays the first result. Other sections are returned non-null/empty so search
/// queries for other content types degrade to "not found" rather than NPE.
async fn search_top_hits(
    State(state): State<ShimState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // Humane's Tidal client passes the phrase as `query`; accept `term` too.
    let term = params
        .get("query")
        .or_else(|| params.get("term"))
        .cloned()
        .unwrap_or_default();
    // `type` selects which section the device will read (ARTISTS/ALBUMS/
    // PLAYLISTS/TRACKS). It resolves an entity for the two-step intents.
    let content_type = params.get("type").map(String::as_str).unwrap_or("TRACKS");
    info!(term = %term, kind = content_type, provider = state.provider().name(), ">>> tidal-shim search/top-hits");

    // Every section must be present (non-null) or the client NPEs.
    let mut artists = empty_section();
    let mut albums = empty_section();
    let mut playlists = empty_section();
    let mut tracks = empty_section();
    let mut top_hits: Vec<Value> = Vec::new();

    match content_type {
        "ARTISTS" => {
            if let Some(a) = state.provider().search_artist(&term).await.ok().flatten() {
                artists = section(vec![artist_json(&a)]);
            } else {
                warn!(term = %term, "no artist match");
            }
        }
        "ALBUMS" => {
            // A `playlist:<mood>` marker means resolve a matching playlist and
            // present it as an album (its id starts with `pl.`, so the tracks
            // endpoint fetches playlist tracks instead of album tracks).
            let entity = if let Some(mood) = term.strip_prefix("playlist:") {
                state.provider().search_playlist(mood.trim()).await.ok().flatten()
            } else {
                state.provider().search_album(&term).await.ok().flatten()
            };
            if let Some(a) = entity {
                albums = section(vec![album_json(&a)]);
            } else {
                warn!(term = %term, "no album match");
            }
        }
        "PLAYLISTS" => {
            if let Some(p) = state.provider().search_playlist(&term).await.ok().flatten() {
                playlists = section(vec![playlist_json_entity(&p)]);
            } else {
                warn!(term = %term, "no playlist match");
            }
        }
        // TRACKS (and anything else): the requested song is the top-hit and
        // plays first, but we follow it with related tracks so the device has a
        // real queue. Otherwise a single-track play leaves the queue empty
        // until the device lazily fetches "up next" near end-of-track, so an
        // early "next" press hits a dead end and stops playback.
        // Personalized-library sentinels ("play my favorites / my playlists /
        // my <name> playlist") route here: resolve the user's real library into
        // a playable queue (first track plays, the rest queue).
        _ if is_personal_term(&term) => {
            let list = resolve_personal(&state, &term).await;
            if let Some(first) = list.first() {
                top_hits.push(json!({ "type": "TRACKS", "value": first.clone() }));
            }
            tracks = section(list);
        }
        _ => {
            match state.provider().search_top(&term).await {
                Ok(Some(song)) => {
                    let seed_id = song.id.clone();
                    let requested = track_json_from_provider(&song);
                    top_hits.push(json!({ "type": "TRACKS", "value": requested.clone() }));
                    let mut list = vec![requested];
                    let related = state
                        .provider()
                        .recommendations(&seed_id, QUEUE_SIZE)
                        .await
                        .unwrap_or_default();
                    for t in keep_playable(&state.provider(), related) {
                        if t.id != seed_id {
                            list.push(track_json_from_provider(&t));
                        }
                    }
                    tracks = section(list);
                }
                _ => {
                    let track = queue_tracks(&state)
                        .await
                        .into_iter()
                        .next()
                        .unwrap_or_else(fallback_track);
                    top_hits.push(json!({ "type": "TRACKS", "value": track.clone() }));
                    tracks = section(vec![track]);
                }
            }
        }
    }

    Json(json!({
        "topHits": top_hits,
        "genres": [],
        "tracks": tracks,
        "albums": albums,
        "artists": artists,
        "playlists": playlists,
        "videos": empty_section()
    }))
}

/// Playback info: a `PlaybackTrackInfo` whose base64 `manifest` (BTS type)
/// carries the provider's playback URL for the track id.
async fn playback_info(State(state): State<ShimState>, Path(id): Path<String>) -> impl IntoResponse {
    // Tidal: pass the device its real `playbackinfopostpaywall` response so its
    // native Tidal player decrypts + plays from Tidal's CDN (real BTS manifest).
    if let Some(real) = state.provider().tidal_playback_info(&id).await {
        if let Ok(track) = state.provider().track(&id).await {
            *state.now_playing.write().await = Some(NowPlaying {
                title: track.title,
                artist: track.artist,
                album: track.album,
            });
        }
        info!(track_id = %id, ">>> tidal-shim playbackinfopostpaywall (real Tidal)");
        return Json(real).into_response();
    }

    // For Mopidy, point its player at this track before handing the device the
    // (constant) Icecast stream URL, so the stream carries the right song.
    state.provider().prepare_playback(&id).await;
    let stream_url = state.provider().playback_url(&id);
    info!(track_id = %id, url = %stream_url, ">>> tidal-shim playbackinfopostpaywall");

    // Record now-playing so the assistant can answer questions about the song.
    if let Ok(track) = state.provider().track(&id).await {
        *state.now_playing.write().await = Some(NowPlaying {
            title: track.title,
            artist: track.artist,
            album: track.album,
        });
    }

    let manifest_json = json!({
        "mimeType": MANIFEST_MIME,
        "codecs": MANIFEST_CODECS,
        "encryptionType": "NONE",
        "urls": [ stream_url ],
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
    .into_response()
}

/// Server-decoded audio stream for non-Apple providers. YouTube resolves the
/// video's HLS audio on-device (so the IP-bound googlevideo URLs are valid for
/// the device) and proxies the segments concatenated into one progressive stream
/// the device's ExoPlayer plays natively. Spotify needs the `spotify-playback`
/// (librespot) feature. Failures return an error status so the device fails soft.
async fn stream_audio(
    State(state): State<ShimState>,
    Path((provider, id)): Path<(String, String)>,
) -> impl IntoResponse {
    info!(provider = %provider, id = %id, "<<< tidal-shim stream request");
    match (provider.as_str(), state.provider().as_ref()) {
        #[cfg(feature = "spotify-playback")]
        ("spotify", MusicProvider::Spotify(client)) => {
            let Some(token) = client.streaming_token() else {
                return (
                    StatusCode::NOT_IMPLEMENTED,
                    "spotify playback needs a `streaming`-scoped token (set [spotify] streaming_token)".to_string(),
                )
                    .into_response();
            };
            match crate::external::spotify_playback::fetch_track_ogg(token, &id).await {
                Ok(bytes) => {
                    info!(bytes = bytes.len(), "<<< tidal-shim spotify (librespot) stream");
                    ([(header::CONTENT_TYPE, "audio/ogg")], bytes).into_response()
                }
                Err(error) => {
                    warn!(%error, "spotify librespot stream failed");
                    (StatusCode::BAD_GATEWAY, error).into_response()
                }
            }
        }
        ("spotify", _) => (
            StatusCode::NOT_IMPLEMENTED,
            "spotify streaming needs the `spotify-playback` (librespot) build feature".to_string(),
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "unknown stream provider".to_string()).into_response(),
    }
}

/// The provider's "play music" queue as Tidal `Track` JSON, with a static
/// fallback so the client never gets an empty list.
async fn queue_tracks(state: &ShimState) -> Vec<Value> {
    match state.provider().queue(QUEUE_SIZE).await {
        Ok(tracks) => {
            let tracks = keep_playable(&state.provider(), tracks);
            if tracks.is_empty() {
                warn!(provider = state.provider().name(), "queue empty after playable filter, using fallback");
                vec![fallback_track()]
            } else {
                tracks.iter().map(track_json_from_provider).collect()
            }
        }
        Err(error) => {
            warn!(provider = state.provider().name(), %error, "provider queue failed, using fallback");
            vec![fallback_track()]
        }
    }
}

/// Randomize track order so personal queues (favorites, "my playlists") don't
/// always start on the same song. Provider-agnostic.
fn shuffle_tracks(mut tracks: Vec<ProviderTrack>) -> Vec<ProviderTrack> {
    use rand::seq::SliceRandom;
    tracks.shuffle(&mut rand::rng());
    tracks
}

/// Resolve a `__penumbra_*` personalized-library sentinel into a playable track
/// list. Works for any provider: Apple serves the real signed-in library;
/// providers without a user library fall back to a shuffled provider queue so
/// the command still plays something. `keep_playable` drops unplayable ids.
async fn resolve_personal(state: &ShimState, term: &str) -> Vec<Value> {
    let provider = state.provider();

    // A specific named playlist plays in its own order (no shuffle).
    if let Some(name) = term.strip_prefix(PLAYLIST_PREFIX) {
        let tracks = keep_playable(&provider, resolve_named_playlist(state, name).await);
        if !tracks.is_empty() {
            return tracks.iter().map(track_json_from_provider).collect();
        }
        warn!(name = %name, "named playlist empty, falling back to chart queue");
        return queue_tracks(state).await;
    }

    // "My playlists": a shuffled mix pulled from the user's library playlists.
    if term == MY_PLAYLISTS_TERM && provider.has_user_library() {
        let mut pooled: Vec<ProviderTrack> = Vec::new();
        if let Ok(playlists) = provider.my_playlists(QUEUE_SIZE).await {
            for pl in playlists.into_iter().take(6) {
                if let Ok(tracks) = provider.my_playlist_tracks(&pl.id, 15).await {
                    pooled.extend(tracks);
                }
                if pooled.len() >= 60 {
                    break;
                }
            }
        }
        let tracks = keep_playable(&provider, shuffle_tracks(pooled));
        if !tracks.is_empty() {
            return tracks.iter().map(track_json_from_provider).collect();
        }
    }

    // Favorites (and any personal term on a provider without a user library):
    // the user's saved songs, shuffled, else a shuffled provider queue.
    let base = if provider.has_user_library() {
        keep_playable(&provider, provider.my_favorites(100).await.unwrap_or_default())
    } else {
        Vec::new()
    };
    if !base.is_empty() {
        return shuffle_tracks(base)
            .iter()
            .map(track_json_from_provider)
            .collect();
    }
    // Fallback: the provider's own queue, shuffled (real "my library" for
    // Spotify/YouTube needs their account login, not yet wired).
    warn!(
        provider = provider.name(),
        term = %term,
        "no user library; falling back to shuffled provider queue"
    );
    let queue = keep_playable(&provider, provider.queue(QUEUE_SIZE).await.unwrap_or_default());
    let queue = shuffle_tracks(queue);
    if queue.is_empty() {
        vec![fallback_track()]
    } else {
        queue.iter().map(track_json_from_provider).collect()
    }
}

/// Find a user/library playlist by (fuzzy) name and return its tracks. Apple
/// checks the signed-in library first; any provider then falls back to a catalog
/// playlist search so "play the &lt;name&gt; playlist" still works.
async fn resolve_named_playlist(state: &ShimState, name: &str) -> Vec<ProviderTrack> {
    let provider = state.provider();
    let wanted = name.trim().to_lowercase();

    if provider.has_user_library() {
        if let Ok(playlists) = provider.my_playlists(QUEUE_SIZE).await {
            let hit = playlists
                .iter()
                .find(|p| p.name.to_lowercase() == wanted)
                .or_else(|| playlists.iter().find(|p| p.name.to_lowercase().contains(&wanted)));
            if let Some(p) = hit {
                if let Ok(tracks) = provider.my_playlist_tracks(&p.id, 100).await {
                    if !tracks.is_empty() {
                        return tracks;
                    }
                }
            }
        }
    }

    // Catalog playlist fallback (all providers).
    if let Ok(Some(entity)) = provider.search_playlist(name).await {
        if let Ok(tracks) = provider.playlist_tracks(&entity.id, 100).await {
            return tracks;
        }
    }
    Vec::new()
}

/// Like [`queue_tracks`] but seeded from a track, so "up next" / radio stays on
/// the requested artist's vibe instead of the generic chart. Falls back to the
/// chart queue if the seed yields nothing playable.
async fn seed_queue_tracks(state: &ShimState, seed_id: &str) -> Vec<Value> {
    match state.provider().recommendations(seed_id, QUEUE_SIZE).await {
        Ok(tracks) => {
            let tracks = keep_playable(&state.provider(), tracks);
            if tracks.is_empty() {
                queue_tracks(state).await
            } else {
                tracks.iter().map(track_json_from_provider).collect()
            }
        }
        Err(error) => {
            warn!(provider = state.provider().name(), %error, "seed recommendations failed, using chart");
            queue_tracks(state).await
        }
    }
}

/// Serves a generated 16-bit PCM mono WAV sine tone over plain HTTP loopback.
/// Legacy redirect→shim→ExoPlayer audio-path test (no provider involved).
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
        "numberOfTracks": QUEUE_SIZE,
        "numberOfVideos": 0,
        "duration": (QUEUE_SIZE * 210),
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

/// Build a Tidal `Track` JSON from a provider track. The Tidal `id` is the
/// provider's id, so `playbackinfopostpaywall/{id}` yields the right URL.
fn track_json_from_provider(t: &ProviderTrack) -> Value {
    let duration_secs = (t.duration_ms / 1000).max(1);
    track_json(&t.id, &t.title, &t.artist, &t.album, duration_secs)
}

fn fallback_track() -> Value {
    track_json(FALLBACK_ID, "Bohemian Rhapsody", "Queen", "A Night at the Opera", 354)
}

/// Drop Apple tracks whose ids overflow the 2021 SDK (see [`crate::music::apple_id_playable`])
/// from multi-track responses so a queue never stalls — only the playable subset reaches the device.
fn keep_playable(provider: &MusicProvider, tracks: Vec<ProviderTrack>) -> Vec<ProviderTrack> {
    match provider {
        // Playable Apple tracks need a numeric catalog id: drop 32-bit-overflow
        // ids (unstreamable via the 2021 SDK) and non-numeric library ids
        // (`i.*`, uncatalogued songs from the personal library) that the catalog
        // player can't load at all.
        MusicProvider::Apple(_) => tracks
            .into_iter()
            .filter(|t| t.id.parse::<u64>().is_ok() && crate::music::apple_id_playable(&t.id))
            .collect(),
        _ => tracks,
    }
}

/// The recurring list container: `{items, limit, offset, totalNumberOfItems}`.
fn section(items: Vec<Value>) -> Value {
    let n = items.len();
    json!({ "items": items, "limit": n, "offset": 0, "totalNumberOfItems": n })
}

fn empty_section() -> Value {
    section(vec![])
}

/// Same shape, as a ready HTTP JSON response.
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

/// Minimal `Artist` (device reads id/name/popularity).
fn artist_json(a: &ProviderEntity) -> Value {
    json!({ "id": a.id, "name": a.name, "popularity": 0, "type": "MAIN", "url": "" })
}

/// Minimal `Album` — `numberOfTracks` must be > 0 or the device skips it.
fn album_json(a: &ProviderEntity) -> Value {
    json!({
        "id": a.id,
        "title": a.name,
        "numberOfTracks": a.track_count.max(1),
        "popularity": 0,
        "artists": [ { "id": "0", "name": "" } ],
        "url": "",
        "cover": null,
        "vibrantColor": null
    })
}

/// Minimal `Playlist` (search result) — keyed by `uuid`, `numberOfTracks` > 0.
fn playlist_json_entity(p: &ProviderEntity) -> Value {
    json!({
        "uuid": p.id,
        "title": p.name,
        "numberOfTracks": p.track_count.max(1),
        "numberOfVideos": 0,
        "duration": (p.track_count.max(1) * 210),
        "promotedArtists": [],
        "publicPlaylist": true,
        "type": "EDITORIAL",
        "url": ""
    })
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

async fn unmatched(request: axum::extract::Request) -> impl IntoResponse {
    warn!(method = %request.method(), path = %request.uri(), "tidal-shim: unimplemented endpoint");
    // JSON object so Gson-based error parsing on the client does not crash.
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({ "status": 404, "subStatus": 404, "userMessage": "tidal-shim: not implemented" })),
    )
}
