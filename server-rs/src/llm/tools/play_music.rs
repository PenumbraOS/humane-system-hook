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
                or start a song, artist, album, genre, or playlist (e.g. 'play Bohemian Rhapsody', \
                'play some Queen', 'play jazz', 'put on my workout playlist'). Fill only the fields \
                the user specified; leave the rest empty. Do NOT use this for general questions."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "track": { "type": "string", "description": "Song / track title, if named." },
                    "artist": { "type": "string", "description": "Artist / band name, if named." },
                    "album": { "type": "string", "description": "Album name, if named." },
                    "genre": { "type": "string", "description": "Genre or mood, if named." }
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
