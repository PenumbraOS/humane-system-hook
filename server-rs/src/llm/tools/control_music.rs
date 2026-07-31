use std::convert::Infallible;

use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolEmbedding};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Signals that the user wants to control the *currently playing* music's
/// transport (pause, resume, skip, etc.). Like [`PlayMusicTool`], this tool is
/// never executed: a rig prompt hook intercepts the call, maps its `action` to
/// the matching Humane device action, and terminates the agent loop so the
/// Understand handler can emit that action instead of a text response.
///
/// [`PlayMusicTool`]: super::play_music::PlayMusicTool
pub struct ControlMusicTool;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ControlMusicToolContext;

impl Tool for ControlMusicTool {
    const NAME: &'static str = "control_music";

    type Error = Infallible;
    type Args = serde_json::Value;
    type Output = serde_json::Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Control music that is already playing (or the user's library) on the \
                device. Use for: transport — pause/stop, resume, skip forward, go back, restart \
                the current track; and library — favorite the current song, play favorites, or \
                start a radio/station based on the current track. This is the tool for 'more like \
                this', 'play more like this', 'play something similar', 'keep this going', and \
                'start a radio' — they all mean action='radio' (a station seeded from whatever is \
                playing now), NOT a request to replay the current song. Do NOT use this to start \
                specific new music by name — use play_music for that."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "pause", "resume", "next", "previous", "restart",
                            "favorite", "play_favorites", "radio"
                        ],
                        "description": "'pause' (also stop), 'resume' (also play/continue), 'next' \
                            (skip forward), 'previous' (go back), 'restart' (current track over), \
                            'favorite' (save/like current track), 'play_favorites' (play liked \
                            songs), 'radio' (start a station from the current track / 'more like \
                            this')."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Unreachable. Intercepted by the prompt hook before execution.
        Ok(json!({}))
    }
}

impl ToolEmbedding for ControlMusicTool {
    type InitError = Infallible;
    type Context = ControlMusicToolContext;
    type State = ();

    fn embedding_docs(&self) -> Vec<String> {
        vec![
            "Control currently playing music: pause, stop, resume, play, skip to the next song, go back to the previous song, or restart the current track.".to_string(),
            "Transport controls for music already playing. Use for 'pause', 'stop', 'resume', 'next', 'skip', 'previous', 'go back', 'start over'.".to_string(),
            "Music library and radio controls: favorite or like the current song, play my favorite/liked songs, or start a radio station based on what's playing ('more like this').".to_string(),
        ]
    }

    fn context(&self) -> Self::Context {
        ControlMusicToolContext
    }

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Self)
    }
}
