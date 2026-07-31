//! Spotify audio streaming via **librespot** (behind the `spotify-playback`
//! feature). Fetches and decrypts a track's audio server-side, then the shim
//! streams it to the device's ExoPlayer — the same server-decode model as the
//! YouTube path, so no on-device DRM is needed.
//!
//! Cross-compiles to `aarch64-linux-android` (validated): sub-crates only
//! (`core`/`audio`/`metadata`, no `librespot-playback` audio backends).
//!
//! Two things are still required for this to actually stream (see the module
//! notes in the repo):
//!   1. A librespot **access token with the `streaming` scope** — this is a
//!      *separate* OAuth from the Web API token (Spotify removed password auth),
//!      obtained via Spotify's desktop client / `librespot-oauth`. Premium only.
//!   2. The device ExoPlayer must accept OGG/Vorbis (Spotify's format).

use std::io::{Read, Seek, SeekFrom};

use librespot_audio::AudioFile;
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_core::spotify_id::SpotifyId;
use librespot_metadata::audio::AudioFileFormat;
use librespot_metadata::{Metadata, Track};

/// Spotify prepends a fixed-size proprietary header before the real OGG stream
/// in the decrypted audio; skip it so ExoPlayer sees a valid Ogg container.
const SPOTIFY_OGG_HEADER: u64 = 0xa7;

/// Preferred OGG/Vorbis formats, best-for-the-Pin first. OGG only, so the fixed
/// header-skip above is always valid.
const FORMAT_PREFERENCE: [AudioFileFormat; 3] = [
    AudioFileFormat::OGG_VORBIS_160,
    AudioFileFormat::OGG_VORBIS_96,
    AudioFileFormat::OGG_VORBIS_320,
];

/// Connect a librespot session with a `streaming`-scoped access token.
async fn connect(access_token: &str) -> Result<Session, String> {
    let session = Session::new(SessionConfig::default(), None);
    let credentials = Credentials::with_access_token(access_token);
    session
        .connect(credentials, false)
        .await
        .map_err(|e| format!("librespot session connect failed: {e}"))?;
    Ok(session)
}

/// Fetch + decrypt a track's audio and return the playable OGG bytes.
/// `track_id` is a base-62 Spotify track id (the shim's provider id).
pub async fn fetch_track_ogg(access_token: &str, track_id: &str) -> Result<Vec<u8>, String> {
    let session = connect(access_token).await?;

    let id = SpotifyId::from_base62(track_id)
        .map_err(|e| format!("bad spotify track id {track_id}: {e:?}"))?;
    let track = Track::get(&session, &id)
        .await
        .map_err(|e| format!("spotify track metadata failed: {e}"))?;

    // Pick the best available audio file for the track.
    let file_id = FORMAT_PREFERENCE
        .iter()
        .find_map(|fmt| track.files.get(fmt).copied())
        .ok_or_else(|| "spotify track has no supported audio format".to_string())?;

    // Bytes-per-second only hints the read-ahead; a reasonable OGG-160 rate.
    let mut audio = AudioFile::open(&session, file_id, 40 * 1024)
        .await
        .map_err(|e| format!("spotify audio open failed: {e}"))?;

    // Skip Spotify's proprietary header so the stream starts at the Ogg magic.
    audio
        .seek(SeekFrom::Start(SPOTIFY_OGG_HEADER))
        .map_err(|e| format!("spotify audio seek failed: {e}"))?;

    let mut out = Vec::new();
    audio
        .read_to_end(&mut out)
        .map_err(|e| format!("spotify audio read failed: {e}"))?;

    // Best-effort session teardown; ignore errors.
    session.shutdown();
    Ok(out)
}
