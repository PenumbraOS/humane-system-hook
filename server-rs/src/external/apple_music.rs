//! Apple Music catalog client (`api.music.apple.com`).
//!
//! Read-only catalog access — song lookup by id and text search — used by the
//! Tidal shim to serve real title/artist/album/artwork instead of PoC
//! placeholders. Only the developer token is required for catalog reads (the
//! Music User Token is for library/playback and lives in the on-device MusicKit
//! hook, not here).

use serde::Deserialize;

const CATALOG_BASE: &str = "https://api.music.apple.com/v1/catalog";
/// Personalized endpoints (library, recommendations, ratings). Every request
/// here additionally requires the `Music-User-Token` header identifying the
/// signed-in account.
const ME_BASE: &str = "https://api.music.apple.com/v1/me";

/// Minimal Apple Music song, flattened from the catalog `attributes`.
#[derive(Debug, Clone)]
pub struct AppleSong {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    /// Artwork URL template (contains `{w}`/`{h}` placeholders), if present.
    /// Reserved for artwork proxying (Tidal's `cover` is a CDN UUID, not a URL).
    #[allow(dead_code)]
    pub artwork_url: Option<String>,
}

/// A catalog entity (artist / album / playlist) — id + display name, and track
/// count when the catalog exposes it (albums/playlists; the device requires a
/// non-zero `numberOfTracks` before it will fetch the collection's tracks).
#[derive(Debug, Clone)]
pub struct AppleEntity {
    pub id: String,
    pub name: String,
    pub track_count: Option<u32>,
}

/// Client for the public Apple Music catalog API. Holds the shared HTTP client,
/// the developer token, and the storefront to query.
#[derive(Clone)]
pub struct AppleMusicClient {
    http: reqwest::Client,
    developer_token: String,
    storefront: String,
    /// Music User Token for the signed-in account, if any. Enables the `/v1/me`
    /// personalized endpoints (library, made-for-you, favorites). Absent =
    /// catalog-only.
    user_token: Option<String>,
}

// ── raw response shapes (only the fields we read) ────────────────────────────

#[derive(Deserialize)]
struct SongsResponse {
    #[serde(default)]
    data: Vec<SongResource>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: SearchResults,
}

#[derive(Deserialize)]
struct ChartsResponse {
    #[serde(default)]
    results: ChartsResults,
}

#[derive(Deserialize, Default)]
struct ChartsResults {
    // `results.songs` here is an ARRAY of chart objects, each with its own `data`.
    #[serde(default)]
    songs: Vec<Chart>,
}

#[derive(Deserialize)]
struct Chart {
    #[serde(default)]
    data: Vec<SongResource>,
}

#[derive(Deserialize, Default)]
struct SearchResults {
    #[serde(default)]
    songs: Option<SongsResponse>,
}

#[derive(Deserialize)]
struct SongResource {
    id: String,
    #[serde(default)]
    attributes: Option<SongAttributes>,
}

#[derive(Deserialize)]
struct SongAttributes {
    #[serde(default)]
    name: String,
    #[serde(rename = "artistName", default)]
    artist_name: String,
    #[serde(rename = "albumName", default)]
    album_name: String,
    #[serde(rename = "durationInMillis", default)]
    duration_in_millis: u64,
    #[serde(default)]
    artwork: Option<Artwork>,
}

#[derive(Deserialize)]
struct Artwork {
    #[serde(default)]
    url: Option<String>,
}

// ---- Personalized `/v1/me` responses (require the Music User Token) ----

#[derive(Deserialize)]
struct LibraryPlaylistsResponse {
    #[serde(default)]
    data: Vec<LibraryPlaylist>,
}

#[derive(Deserialize)]
struct LibraryPlaylist {
    id: String,
    #[serde(default)]
    attributes: Option<LibraryPlaylistAttributes>,
}

#[derive(Deserialize)]
struct LibraryPlaylistAttributes {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct LibrarySongsResponse {
    #[serde(default)]
    data: Vec<LibrarySong>,
}

#[derive(Deserialize)]
struct LibrarySong {
    id: String,
    #[serde(default)]
    attributes: Option<LibrarySongAttributes>,
}

#[derive(Deserialize)]
struct LibrarySongAttributes {
    #[serde(default)]
    name: String,
    #[serde(rename = "artistName", default)]
    artist_name: Option<String>,
    #[serde(rename = "albumName", default)]
    album_name: Option<String>,
    #[serde(rename = "durationInMillis", default)]
    duration_ms: Option<u64>,
    #[serde(rename = "playParams", default)]
    play_params: Option<LibraryPlayParams>,
}

#[derive(Deserialize)]
struct LibraryPlayParams {
    #[serde(rename = "catalogId", default)]
    catalog_id: Option<String>,
}

/// Flatten library-song resources into [`AppleSong`]s, preferring the catalog id
/// (from `playParams`) so the track plays through the catalog player; songs with
/// only a library id (`i.*`, uncatalogued) keep it and are filtered out of
/// playback queues downstream.
fn library_songs_into(data: Vec<LibrarySong>) -> Vec<AppleSong> {
    data.into_iter()
        .filter_map(|s| {
            let a = s.attributes?;
            Some(AppleSong {
                id: a.play_params.and_then(|p| p.catalog_id).unwrap_or(s.id),
                name: a.name,
                artist: a.artist_name.unwrap_or_default(),
                album: a.album_name.unwrap_or_default(),
                duration_ms: a.duration_ms.unwrap_or(0),
                artwork_url: None,
            })
        })
        .collect()
}

impl SongResource {
    fn into_song(self) -> AppleSong {
        let a = self.attributes.unwrap_or(SongAttributes {
            name: String::new(),
            artist_name: String::new(),
            album_name: String::new(),
            duration_in_millis: 0,
            artwork: None,
        });
        AppleSong {
            id: self.id,
            name: a.name,
            artist: a.artist_name,
            album: a.album_name,
            duration_ms: a.duration_in_millis,
            artwork_url: a.artwork.and_then(|art| art.url),
        }
    }
}

impl AppleMusicClient {
    pub fn new(http: reqwest::Client, developer_token: String, storefront: String) -> Self {
        Self {
            http,
            developer_token,
            storefront,
            user_token: None,
        }
    }

    /// Attach a Music User Token, enabling the `/v1/me` personalized endpoints.
    pub fn with_user_token(mut self, user_token: Option<String>) -> Self {
        self.user_token = user_token.filter(|t| !t.is_empty());
        self
    }

    /// Whether a Music User Token is configured (personalized calls available).
    pub fn has_user_token(&self) -> bool {
        self.user_token.is_some()
    }

    /// GET a personalized `/v1/me` endpoint. Adds the `Music-User-Token` header
    /// on top of the developer bearer token. `path` is appended to [`ME_BASE`].
    async fn get_me<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, String> {
        let Some(user_token) = self.user_token.as_deref() else {
            return Err("apple: no Music-User-Token configured".to_string());
        };
        let url = format!("{ME_BASE}{path}");
        let full_url = reqwest::Url::parse_with_params(&url, query)
            .map_err(|e| format!("apple me url build failed: {e}"))?;
        let resp = self
            .http
            .get(full_url)
            .bearer_auth(&self.developer_token)
            .header("Music-User-Token", user_token)
            .send()
            .await
            .map_err(|e| format!("apple me request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("apple me {status}: {text}"));
        }
        resp.json::<T>()
            .await
            .map_err(|e| format!("apple me decode failed: {e}"))
    }

    /// The user's library playlists (`/v1/me/library/playlists`) — id + name +
    /// track count, for "play my playlists" / "play my <playlist>".
    pub async fn library_playlists(&self, limit: u32) -> Result<Vec<AppleEntity>, String> {
        let limit = limit.clamp(1, 100).to_string();
        let body: LibraryPlaylistsResponse =
            self.get_me("/library/playlists", &[("limit", &limit)]).await?;
        Ok(body
            .data
            .into_iter()
            .map(|p| AppleEntity {
                id: p.id,
                name: p.attributes.map(|a| a.name).unwrap_or_default(),
                track_count: None,
            })
            .collect())
    }

    /// The user's favorite / library songs (`/v1/me/library/songs`), newest
    /// first — for "play my favorites" / "play my liked songs".
    pub async fn library_songs(&self, limit: u32) -> Result<Vec<AppleSong>, String> {
        let limit = limit.clamp(1, 100).to_string();
        let body: LibrarySongsResponse = self
            .get_me("/library/songs", &[("limit", &limit)])
            .await?;
        Ok(library_songs_into(body.data))
    }

    /// Tracks of one of the user's library playlists
    /// (`/v1/me/library/playlists/{id}/tracks`), in order.
    pub async fn library_playlist_tracks(
        &self,
        playlist_id: &str,
        limit: u32,
    ) -> Result<Vec<AppleSong>, String> {
        let limit = limit.clamp(1, 100).to_string();
        let path = format!("/library/playlists/{playlist_id}/tracks");
        let body: LibrarySongsResponse = self.get_me(&path, &[("limit", &limit)]).await?;
        Ok(library_songs_into(body.data))
    }

    /// Look up a single catalog song by id. Kept as client API / test coverage;
    /// the shim currently plays from queue/search results without re-fetching.
    #[allow(dead_code)]
    pub async fn song(&self, id: &str) -> Result<AppleSong, String> {
        let url = format!("{CATALOG_BASE}/{}/songs/{}", self.storefront, id);
        let body: SongsResponse = self.get(&url, &[]).await?;
        body.data
            .into_iter()
            .next()
            .map(SongResource::into_song)
            .ok_or_else(|| format!("apple catalog: song {id} not found"))
    }

    /// Search the catalog and return the top song match, if any.
    pub async fn search_top_song(&self, term: &str) -> Result<Option<AppleSong>, String> {
        Ok(self.search_songs(term, 1).await?.into_iter().next())
    }

    /// Search catalog songs (used for track search and genre/mood queries).
    pub async fn search_songs(&self, term: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!("{CATALOG_BASE}/{}/search", self.storefront);
        // Apple caps the search `limit` at 25; a larger value is a 400.
        let limit = limit.clamp(1, 25).to_string();
        let body: SearchResponse = self
            .get(&url, &[("term", term), ("types", "songs"), ("limit", &limit)])
            .await?;
        Ok(body
            .results
            .songs
            .map(|s| s.data.into_iter().map(SongResource::into_song).collect())
            .unwrap_or_default())
    }

    /// Top catalog songs from the storefront's chart — used to build a real
    /// multi-track queue for "play music" and to keep playback going when the
    /// queue runs dry.
    pub async fn chart_songs(&self, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!("{CATALOG_BASE}/{}/charts", self.storefront);
        let limit = limit.to_string();
        let body: ChartsResponse = self
            .get(&url, &[("types", "songs"), ("limit", &limit)])
            .await?;
        Ok(body
            .results
            .songs
            .into_iter()
            .next()
            .map(|chart| chart.data.into_iter().map(SongResource::into_song).collect())
            .unwrap_or_default())
    }

    /// Top chart songs for a specific genre id (real genre "station").
    pub async fn genre_chart_songs(&self, genre_id: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!("{CATALOG_BASE}/{}/charts", self.storefront);
        let limit = limit.clamp(1, 25).to_string();
        let body: ChartsResponse = self
            .get(&url, &[("types", "songs"), ("genre", genre_id), ("limit", &limit)])
            .await?;
        Ok(body
            .results
            .songs
            .into_iter()
            .next()
            .map(|chart| chart.data.into_iter().map(SongResource::into_song).collect())
            .unwrap_or_default())
    }

    /// Find the top catalog entity of `kind` ("artists" | "albums" | "playlists")
    /// matching `term`. Returns its id + display name.
    pub async fn search_entity(&self, kind: &str, term: &str) -> Result<Option<AppleEntity>, String> {
        let url = format!("{CATALOG_BASE}/{}/search", self.storefront);
        let body: serde_json::Value = self
            .get(&url, &[("term", term), ("types", kind), ("limit", "1")])
            .await?;
        let item = body
            .pointer(&format!("/results/{kind}/data/0"))
            .and_then(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                let name = v
                    .pointer("/attributes/name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let track_count = v
                    .pointer("/attributes/trackCount")
                    .and_then(|n| n.as_u64())
                    .map(|n| n as u32);
                Some(AppleEntity {
                    id,
                    name,
                    track_count,
                })
            });
        Ok(item)
    }

    /// The artist's newest full album title (prefers albums over singles/EPs).
    pub async fn artist_latest_album(&self, artist_id: &str) -> Result<Option<String>, String> {
        let url = format!("{CATALOG_BASE}/{}/artists/{artist_id}/albums", self.storefront);
        let body: serde_json::Value = self.get(&url, &[("limit", "25")]).await?;
        let mut albums: Vec<(String, String, u64)> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        let name = a.pointer("/attributes/name")?.as_str()?.to_string();
                        let date = a
                            .pointer("/attributes/releaseDate")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string();
                        let tracks = a
                            .pointer("/attributes/trackCount")
                            .and_then(|t| t.as_u64())
                            .unwrap_or(0);
                        Some((name, date, tracks))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Newest first by release date.
        albums.sort_by(|a, b| b.1.cmp(&a.1));
        // Prefer a real album (>3 tracks, not a "- Single"); else newest overall.
        Ok(albums
            .iter()
            .find(|(name, _, tracks)| *tracks > 3 && !name.ends_with("- Single"))
            .or_else(|| albums.first())
            .map(|(name, _, _)| name.clone()))
    }

    /// An artist's top songs.
    pub async fn artist_top_songs(&self, artist_id: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!(
            "{CATALOG_BASE}/{}/artists/{artist_id}/view/top-songs",
            self.storefront
        );
        self.songs_at(&url, limit).await
    }

    /// An album's tracks, in album order.
    pub async fn album_tracks(&self, album_id: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!("{CATALOG_BASE}/{}/albums/{album_id}/tracks", self.storefront);
        self.songs_at(&url, limit).await
    }

    /// A playlist's tracks.
    pub async fn playlist_tracks(&self, playlist_id: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let url = format!(
            "{CATALOG_BASE}/{}/playlists/{playlist_id}/tracks",
            self.storefront
        );
        self.songs_at(&url, limit).await
    }

    /// GET a `{data:[song,...]}` endpoint and map to songs.
    async fn songs_at(&self, url: &str, limit: u32) -> Result<Vec<AppleSong>, String> {
        let limit = limit.to_string();
        let body: SongsResponse = self.get(url, &[("limit", &limit)]).await?;
        Ok(body.data.into_iter().map(SongResource::into_song).collect())
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T, String> {
        let full_url = reqwest::Url::parse_with_params(url, query)
            .map_err(|e| format!("apple catalog url build failed: {e}"))?;
        let resp = self
            .http
            .get(full_url)
            .bearer_auth(&self.developer_token)
            .send()
            .await
            .map_err(|e| format!("apple catalog request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("apple catalog {status}: {text}"));
        }
        resp.json::<T>()
            .await
            .map_err(|e| format!("apple catalog decode failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Option<AppleMusicClient> {
        let token = std::env::var("APPLE_DEV_TOKEN").ok()?;
        Some(AppleMusicClient::new(
            reqwest::Client::new(),
            token,
            "us".to_string(),
        ))
    }

    // Phase 0 spike: confirm the Music User Token unlocks `/v1/me/*` from the
    // server. Run explicitly with both tokens:
    //   APPLE_DEV_TOKEN=... APPLE_USER_TOKEN=... \
    //     cargo test apple_music spike_me -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn spike_me_endpoints() {
        let (Ok(dev), Ok(user)) = (
            std::env::var("APPLE_DEV_TOKEN"),
            std::env::var("APPLE_USER_TOKEN"),
        ) else {
            eprintln!("APPLE_DEV_TOKEN / APPLE_USER_TOKEN unset; skipping");
            return;
        };
        let c = AppleMusicClient::new(reqwest::Client::new(), dev, "us".to_string())
            .with_user_token(Some(user));
        assert!(c.has_user_token());

        match c.library_playlists(25).await {
            Ok(pls) => {
                eprintln!("library playlists: {}", pls.len());
                for p in pls.iter().take(10) {
                    eprintln!("  - {} ({})", p.name, p.id);
                }
            }
            Err(e) => eprintln!("library_playlists ERROR: {e}"),
        }

        match c.library_songs(25).await {
            Ok(songs) => {
                eprintln!("library songs: {}", songs.len());
                for s in songs.iter().take(10) {
                    eprintln!("  - {} — {} (id {})", s.name, s.artist, s.id);
                }
            }
            Err(e) => eprintln!("library_songs ERROR: {e}"),
        }
    }

    // Network + token; run explicitly:
    //   APPLE_DEV_TOKEN=... cargo test apple_music -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn song_lookup_returns_real_metadata() {
        let Some(c) = client() else {
            eprintln!("APPLE_DEV_TOKEN unset; skipping");
            return;
        };
        let song = c.song("1440650711").await.expect("song lookup");
        assert_eq!(song.id, "1440650711");
        assert_eq!(song.name, "Bohemian Rhapsody");
        assert_eq!(song.artist, "Queen");
        assert!(song.duration_ms > 0);
    }

    #[tokio::test]
    #[ignore]
    async fn artist_intent_resolves_top_songs() {
        let Some(c) = client() else { return };
        let artist = c.search_entity("artists", "Rihanna").await.expect("search").expect("artist");
        assert_eq!(artist.name, "Rihanna");
        let songs = c.artist_top_songs(&artist.id, 5).await.expect("top songs");
        // Top songs include collabs (primary artist may differ), so just assert
        // we got real, named tracks back from the artist's catalog view.
        assert!(songs.len() >= 3);
        assert!(songs.iter().all(|s| !s.name.is_empty() && s.id.len() > 3));
    }

    #[tokio::test]
    #[ignore]
    async fn resolves_artist_latest_album() {
        let Some(c) = client() else { return };
        let artist = c.search_entity("artists", "Kanye West").await.unwrap().unwrap();
        let album = c.artist_latest_album(&artist.id).await.unwrap();
        eprintln!("Kanye latest album -> {album:?}");
        let album = album.expect("an album");
        assert!(!album.is_empty());
        // His newest should not be a 2004 debut track.
        assert_ne!(album, "The College Dropout");
    }

    #[tokio::test]
    #[ignore]
    async fn album_intent_resolves_tracks_in_order() {
        let Some(c) = client() else { return };
        let album = c
            .search_entity("albums", "Thriller Michael Jackson")
            .await
            .expect("search")
            .expect("album");
        let tracks = c.album_tracks(&album.id, 20).await.expect("tracks");
        assert!(tracks.len() > 3);
        assert!(tracks.iter().any(|t| t.name.contains("Thriller")));
    }

    #[tokio::test]
    #[ignore]
    async fn search_resolves_top_song() {
        let Some(c) = client() else {
            eprintln!("APPLE_DEV_TOKEN unset; skipping");
            return;
        };
        let song = c
            .search_top_song("bohemian rhapsody queen")
            .await
            .expect("search")
            .expect("a match");
        assert!(song.name.to_lowercase().contains("bohemian"));
        assert_eq!(song.artist, "Queen");
    }
}
