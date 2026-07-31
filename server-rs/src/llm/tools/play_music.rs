use std::convert::Infallible;

use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolEmbedding};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Signals that the user wants to play music. Like [`UnderstandSceneTool`], this
/// tool is never actually executed: a rig prompt hook intercepts the call,
/// captures its arguments, and terminates the agent loop so the Understand
/// handler can emit a device `PlayMusic` action instead of a text response.
///
/// [`UnderstandSceneTool`]: super::understand_scene::UnderstandSceneTool
pub struct PlayMusicTool;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlayMusicToolContext;

impl Tool for PlayMusicTool {
    const NAME: &'static str = "play_music";

    type Error = Infallible;
    type Args = serde_json::Value;
    type Output = serde_json::Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Play music on the device. Use whenever the user asks to play, put on, \
                or start music (e.g. 'play Bohemian Rhapsody', 'play some Queen', 'play jazz', \
                'play something chill', 'put on Afro House', 'play my workout playlist', 'play \
                Kanye's new album'). Fill only the fields the user specified; leave the rest \
                empty. Resolve STRICTLY from the user's latest/most-recent message — never reuse \
                a song, artist, album, or mood mentioned earlier in the conversation; each play \
                request is independent. Do NOT use this for general questions or to control \
                already-playing music (use control_music for pause/skip/etc). In particular, \
                'more like this', 'play more like this', 'play something similar', 'keep this vibe \
                going', 'start a radio', 'skip'/'next', and 'previous' are NOT play_music requests \
                — they operate on the current track, so use control_music. Only use play_music \
                when the user names new music (a specific song, artist, album, genre, or mood)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "track": { "type": "string", "description": "A specific song / track title, if named." },
                    "artist": { "type": "string", "description": "Artist / band name, if named." },
                    "album": { "type": "string", "description": "A specific album name, if named." },
                    "mood": {
                        "type": "string",
                        "description": "A genre, mood, activity, era, or vibe rather than a specific \
                            song/artist/album — e.g. 'afro house', 'rap', 'jazz', 'chill', \
                            'workout', 'sad songs', '80s'. Plays a matching playlist."
                    },
                    "playlist": {
                        "type": "string",
                        "description": "One of the USER'S OWN saved/library playlists, when they say \
                            'my ...' — e.g. 'play my workout playlist' -> 'workout', 'play my \
                            Discovery playlist' -> 'Discovery'. Use 'my playlists' (verbatim) when \
                            they just say 'play my playlists' with no specific name. Prefer this \
                            over `mood` whenever the user says 'my'."
                    },
                    "latest_album": {
                        "type": "boolean",
                        "description": "True when the user wants the named artist's NEWEST/latest \
                            album (set `artist` too), e.g. 'play Kanye's new album'."
                    }
                },
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Unreachable. Intercepted by the prompt hook before execution.
        Ok(json!({}))
    }
}

impl ToolEmbedding for PlayMusicTool {
    type InitError = Infallible;
    type Context = PlayMusicToolContext;
    type State = ();

    fn embedding_docs(&self) -> Vec<String> {
        vec![
            "Play music on the device: start a song, artist, album, genre, playlist, or radio. Use for 'play <song>', 'play some <artist>', 'put on <album>', 'play <genre> music', 'play my <playlist>'.".to_string(),
            "Start audio playback of requested music. Good for requests to play, put on, or listen to a specific track, band, album, or style of music.".to_string(),
            "Handle music playback requests by name: songs, artists, albums, genres, moods, or playlists the user wants to hear right now.".to_string(),
        ]
    }

    fn context(&self) -> Self::Context {
        PlayMusicToolContext
    }

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Self)
    }
}
