//! YouTube Music client — real InnerTube search + on-device audio resolution.
//!
//! No account needed. Search uses the `WEB_REMIX` (YouTube Music) InnerTube
//! endpoint. Playback: the `IOS` player client returns an HLS manifest whose
//! audio renditions are NOT PO-token/cipher gated (unlike the progressive
//! `adaptiveFormats`, which omit their URL without a PO token). Humane's ExoPlayer
//! ships the progressive extractors (ADTS/MP4/fMP4/TS) but NOT the HLS
//! `MediaSource`, so the shim proxies the HLS audio *segments* as one progressive
//! stream (see `tidal_shim::stream_audio`) which ExoPlayer plays natively.
//!
//! googlevideo URLs are IP-bound to whoever made the player request — since the
//! server runs on the device (same IP as ExoPlayer), resolving here is what makes
//! the stream playable.

use serde_json::{json, Value};

use crate::music::ProviderTrack;

const MUSIC_SEARCH: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
const PLAYER: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
const WEB_REMIX_VERSION: &str = "1.20260707.12.00";
const IOS_VERSION: &str = "21.26.4";
const IOS_UA: &str = "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_5 like Mac OS X)";
/// InnerTube search filter params that restrict results to songs.
const SONGS_PARAMS: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
/// Broad query used to fill the "play music" (no search term) queue.
const QUEUE_QUERY: &str = "top songs today";

/// Resolved audio to proxy: the ordered segment URLs (init segment first, if the
/// stream is fragmented MP4) and the container Content-Type.
pub struct ResolvedAudio {
    pub content_type: &'static str,
    pub segments: Vec<String>,
}

pub struct YoutubeClient {
    http: reqwest::Client,
    #[allow(dead_code)]
    cookies_path: Option<String>,
}

impl YoutubeClient {
    pub fn new(http: reqwest::Client, cookies_path: Option<String>) -> Self {
        Self { http, cookies_path }
    }

    /// Shared HTTP client for the shim's segment-proxy stream.
    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    pub async fn search_top(&self, term: &str) -> Result<Option<ProviderTrack>, String> {
        Ok(self.search(term, 1).await?.into_iter().next())
    }

    /// Search catalog songs (used for artist/album/genre intents).
    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        self.search(term, limit.max(1)).await
    }

    pub async fn queue(&self, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        self.search(QUEUE_QUERY, limit).await
    }

    async fn search(&self, term: &str, limit: u32) -> Result<Vec<ProviderTrack>, String> {
        let body = json!({
            "context": { "client": {
                "clientName": "WEB_REMIX", "clientVersion": WEB_REMIX_VERSION,
                "hl": "en", "gl": "US"
            }},
            "query": term,
            "params": SONGS_PARAMS,
        });
        let resp = self
            .http
            .post(MUSIC_SEARCH)
            .header("Origin", "https://music.youtube.com")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("youtube search request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("youtube search {}", resp.status()));
        }
        let value: Value = resp
            .json()
            .await
            .map_err(|e| format!("youtube search decode failed: {e}"))?;

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        collect_songs(&value, &mut out, &mut seen);
        out.truncate(limit as usize);
        Ok(out)
    }

    /// Resolve a playable audio stream for a video id via the IOS HLS manifest.
    /// Returns the ordered segment URLs (init first for fMP4) to proxy.
    pub async fn resolve_audio(&self, id: &str) -> Result<ResolvedAudio, String> {
        let body = json!({
            "context": { "client": {
                "clientName": "IOS", "clientVersion": IOS_VERSION,
                "deviceMake": "Apple", "deviceModel": "iPhone16,2",
                "hl": "en", "gl": "US"
            }},
            "videoId": id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let player: Value = self
            .http
            .post(PLAYER)
            .header("User-Agent", IOS_UA)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("youtube player request failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("youtube player decode failed: {e}"))?;

        let status = player
            .pointer("/playabilityStatus/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if status != "OK" {
            let reason = player
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or(status);
            return Err(format!("youtube not playable: {reason}"));
        }
        let master_url = player
            .pointer("/streamingData/hlsManifestUrl")
            .and_then(Value::as_str)
            .ok_or("youtube: no HLS manifest (PO token gated)")?;

        let master = self.get_text(master_url).await?;
        let media_url = choose_audio_playlist(&master)
            .ok_or("youtube: no audio playlist in HLS manifest")?;
        let media = self.get_text(&media_url).await?;
        let (content_type, mut segments) = parse_media_playlist(&media);
        if segments.is_empty() {
            return Err("youtube: HLS media playlist had no segments".to_string());
        }
        // init segment (if fMP4) is already prepended by parse_media_playlist.
        segments.shrink_to_fit();
        Ok(ResolvedAudio {
            content_type,
            segments,
        })
    }

    async fn get_text(&self, url: &str) -> Result<String, String> {
        self.http
            .get(url)
            .header("User-Agent", IOS_UA)
            .send()
            .await
            .map_err(|e| format!("youtube manifest fetch failed: {e}"))?
            .text()
            .await
            .map_err(|e| format!("youtube manifest read failed: {e}"))
    }
}

// ── InnerTube search parsing (tolerant tree walk) ────────────────────────────

fn collect_songs(v: &Value, out: &mut Vec<ProviderTrack>, seen: &mut std::collections::HashSet<String>) {
    match v {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(track) = parse_song_item(item) {
                    if seen.insert(track.id.clone()) {
                        out.push(track);
                    }
                }
            }
            for val in map.values() {
                collect_songs(val, out, seen);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                collect_songs(val, out, seen);
            }
        }
        _ => {}
    }
}

fn parse_song_item(item: &Value) -> Option<ProviderTrack> {
    let id = first_string(item, "videoId")?;
    if id.len() != 11 {
        return None;
    }
    let cols = item.get("flexColumns").and_then(Value::as_array);
    let col_texts = |i: usize| -> Vec<String> {
        cols.and_then(|c| c.get(i))
            .and_then(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs"))
            .and_then(Value::as_array)
            .map(|runs| {
                runs.iter()
                    .filter_map(|r| r.get("text").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let title = col_texts(0).join("");
    let detail: Vec<String> = col_texts(1)
        .into_iter()
        .filter(|t| t.trim() != "•" && !t.trim().is_empty())
        .collect();
    let artist = detail.first().cloned().unwrap_or_default();
    let duration_ms = detail
        .iter()
        .find_map(|t| parse_duration(t))
        .unwrap_or(0);
    // Album: a middle field that isn't the artist, a duration, or a count.
    let album = detail
        .iter()
        .skip(1)
        .find(|t| parse_duration(t).is_none() && !is_count(t))
        .cloned()
        .unwrap_or_default();

    Some(ProviderTrack {
        id,
        title,
        artist,
        album,
        duration_ms,
    })
}

/// First string value under `key` anywhere in the subtree.
fn first_string(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            map.values().find_map(|val| first_string(val, key))
        }
        Value::Array(arr) => arr.iter().find_map(|val| first_string(val, key)),
        _ => None,
    }
}

fn parse_duration(t: &str) -> Option<u64> {
    let t = t.trim();
    if !t.contains(':') {
        return None;
    }
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut secs: u64 = 0;
    for p in &parts {
        let n: u64 = p.parse().ok()?;
        secs = secs * 60 + n;
    }
    Some(secs * 1000)
}

fn is_count(t: &str) -> bool {
    let l = t.to_lowercase();
    l.contains("view") || l.contains("play") || l.contains("listener") || l.contains("stream")
}

// ── HLS parsing ──────────────────────────────────────────────────────────────

/// Pick the best media playlist from an HLS master: prefer a separate audio-only
/// rendition (highest itag); else the lowest-bandwidth variant (muxed, small).
fn choose_audio_playlist(master: &str) -> Option<String> {
    let mut best_audio: Option<(u32, String)> = None;
    for line in master.lines() {
        if line.starts_with("#EXT-X-MEDIA:") && line.contains("TYPE=AUDIO") {
            if let Some(uri) = attr(line, "URI") {
                let itag = itag_of(&uri);
                if best_audio.as_ref().map(|(i, _)| itag > *i).unwrap_or(true) {
                    best_audio = Some((itag, uri));
                }
            }
        }
    }
    if let Some((_, uri)) = best_audio {
        return Some(uri);
    }
    // Fall back to lowest-bandwidth variant.
    let mut best_variant: Option<(u64, String)> = None;
    let lines: Vec<&str> = master.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-STREAM-INF") {
            let bw = line
                .split("BANDWIDTH=")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            if let Some(uri) = lines[i + 1..]
                .iter()
                .find(|l| !l.starts_with('#') && !l.trim().is_empty())
            {
                if best_variant.as_ref().map(|(b, _)| bw < *b).unwrap_or(true) {
                    best_variant = Some((bw, uri.trim().to_string()));
                }
            }
        }
    }
    best_variant.map(|(_, uri)| uri)
}

/// Returns (content_type, segments) where segments begins with the fMP4 init
/// segment when present.
fn parse_media_playlist(media: &str) -> (&'static str, Vec<String>) {
    let mut segments = Vec::new();
    let mut fragmented = false;
    for line in media.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            fragmented = true;
            if let Some(uri) = attr(rest, "URI") {
                segments.push(uri); // init first
            }
        } else if line.starts_with("http") {
            segments.push(line.to_string());
        }
    }
    let content_type = if fragmented { "audio/mp4" } else { "video/mp2t" };
    (content_type, segments)
}

/// Extract an `KEY="value"` attribute from an HLS tag line.
fn attr(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn itag_of(url: &str) -> u32 {
    url.split("/itag/")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hls_attr_and_itag() {
        assert_eq!(attr(r#"#EXT-X-MAP:URI="https://x/y""#, "URI").as_deref(), Some("https://x/y"));
        assert_eq!(itag_of("https://h/api/manifest/hls_playlist/itag/140/source/youtube"), 140);
    }

    #[test]
    fn parses_duration() {
        assert_eq!(parse_duration("3:33"), Some(213_000));
        assert_eq!(parse_duration("1:02:03"), Some(3_723_000));
        assert_eq!(parse_duration("Queen"), None);
    }

    #[test]
    fn ts_vs_fmp4_content_type() {
        let ts = "#EXTM3U\nhttps://seg1.ts\nhttps://seg2.ts\n";
        assert_eq!(parse_media_playlist(ts).0, "video/mp2t");
        let fmp4 = "#EXTM3U\n#EXT-X-MAP:URI=\"https://init.mp4\"\nhttps://seg1.mp4\n";
        let (ct, segs) = parse_media_playlist(fmp4);
        assert_eq!(ct, "audio/mp4");
        assert_eq!(segs[0], "https://init.mp4"); // init prepended
    }

    fn client() -> YoutubeClient {
        YoutubeClient::new(reqwest::Client::new(), None)
    }

    // Network. Run: cargo test youtube -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_search_returns_songs() {
        let c = client();
        let song = c
            .search_top("bohemian rhapsody queen")
            .await
            .expect("search")
            .expect("a match");
        eprintln!("search -> {} | {} — {} ({}ms)", song.id, song.title, song.artist, song.duration_ms);
        assert_eq!(song.id.len(), 11);
        assert!(song.title.to_lowercase().contains("bohemian"));
    }

    // Network. Resolves + downloads + concatenates the HLS audio, writes it to a
    // temp file, and asserts it's a non-trivial audio blob (ffprobe if present).
    #[tokio::test]
    #[ignore]
    async fn live_resolve_and_download_audio() {
        let c = client();
        let audio = c.resolve_audio("dQw4w9WgXcQ").await.expect("resolve");
        eprintln!("resolved {} segments, content-type {}", audio.segments.len(), audio.content_type);
        assert!(!audio.segments.is_empty());
        let mut buf = Vec::new();
        for url in &audio.segments {
            let bytes = c.http().get(url).send().await.unwrap().bytes().await.unwrap();
            buf.extend_from_slice(&bytes);
        }
        eprintln!("downloaded {} bytes of {}", buf.len(), audio.content_type);
        assert!(buf.len() > 100_000, "audio should be a real track, got {} bytes", buf.len());
        let path = std::env::temp_dir().join("yt_audio_test.bin");
        std::fs::write(&path, &buf).unwrap();
        eprintln!("wrote {}", path.display());
    }

    #[test]
    fn chooses_audio_rendition_over_variant() {
        let master = concat!(
            "#EXTM3U\n",
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"233\",URI=\"https://a/itag/139/x\"\n",
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"234\",URI=\"https://a/itag/140/x\"\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=300000,AUDIO=\"234\"\n",
            "https://variant/itag/230/x\n",
        );
        assert_eq!(choose_audio_playlist(master).as_deref(), Some("https://a/itag/140/x"));
    }
}
