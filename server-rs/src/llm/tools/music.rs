//! Playback control for the assistant.
//!
//! The music app owns playback; the server only feeds it content through
//! [`tidal_shim`](crate::services::tidal_shim). So this tool does not play
//! audio itself — it drives the app the way a media button would, through
//! `media dispatch` for transport and the standard `MEDIA_PLAY_FROM_SEARCH`
//! intent to start something new.
//!
//! Without this the assistant has no music capability at all, and a request
//! like "change the song" falls through to a plain "I can't play music".

use std::convert::Infallible;

use rig::tool::{Tool, ToolContext, ToolEmbedding};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[cfg(target_os = "android")]
use tokio::process::Command;

/// Transport actions, mapped to what `media dispatch` accepts.
const DISPATCH: &[(&str, &str)] = &[
    ("next", "next"),
    ("previous", "previous"),
    ("pause", "pause"),
    ("resume", "play"),
    ("stop", "stop"),
];

#[derive(Debug, Clone)]
pub struct MusicControlTool;

#[derive(Debug, Deserialize)]
pub struct MusicControlArgs {
    /// One of: play, next, previous, pause, resume, stop.
    pub action: String,
    /// What to play. Only read for `action: "play"`.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MusicControlResult {
    pub result: String,
}

#[derive(Debug)]
pub struct MusicControlError(String);

impl std::fmt::Display for MusicControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MusicControlError {}

/// Run a device command, mapping a non-zero exit to an error the model can read.
#[cfg(target_os = "android")]
async fn run(bin: &str, args: &[&str]) -> Result<(), MusicControlError> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .await
        .map_err(|e| MusicControlError(format!("failed to spawn {bin}: {e}")))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(MusicControlError(format!(
        "{bin} exited {status}: {stderr}",
        status = output.status
    )))
}

#[cfg(not(target_os = "android"))]
async fn run(_bin: &str, _args: &[&str]) -> Result<(), MusicControlError> {
    Err(MusicControlError(
        "music control is only available on the device".to_string(),
    ))
}

impl Tool for MusicControlTool {
    const NAME: &'static str = "control_music";

    type Error = MusicControlError;
    type Args = MusicControlArgs;
    type Output = MusicControlResult;

    fn description(&self) -> String {
        "Control music playback on this device. Use this to start playing a song, \
         artist, album or genre, to skip to the next track, go back to the previous \
         track, pause, resume, or stop the music. Use it whenever the user wants to \
         play music, change the song, skip what is playing, put something else on, \
         or stop or resume playback."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["play", "next", "previous", "pause", "resume", "stop"],
                    "description": "What to do. Use 'play' with a query to start \
                         something specific; 'next' to skip or change the song."
                },
                "query": {
                    "type": "string",
                    "description": "The song, artist, album or genre to play. \
                         Required when action is 'play'; ignored otherwise."
                }
            },
            "required": ["action"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let action = args.action.trim().to_lowercase();

        if action == "play" {
            let query = args
                .query
                .as_deref()
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| {
                    MusicControlError("play needs a query naming what to play".to_string())
                })?;

            // The standard Android "play this" intent. The music app handles the
            // search itself, which is what a spoken request already goes through.
            run(
                "/system/bin/am",
                &[
                    "start",
                    "-a",
                    "android.media.action.MEDIA_PLAY_FROM_SEARCH",
                    "-e",
                    "query",
                    query,
                ],
            )
            .await?;

            return Ok(MusicControlResult {
                result: format!("Started playing {query}."),
            });
        }

        let Some((_, dispatch)) = DISPATCH.iter().find(|(name, _)| *name == action) else {
            return Err(MusicControlError(format!(
                "unknown action '{action}'; expected play, next, previous, pause, resume or stop"
            )));
        };

        run("/system/bin/media", &["dispatch", dispatch]).await?;

        Ok(MusicControlResult {
            result: match action.as_str() {
                "next" => "Skipped to the next track.".to_string(),
                "previous" => "Went back to the previous track.".to_string(),
                "pause" => "Paused the music.".to_string(),
                "resume" => "Resumed the music.".to_string(),
                _ => "Stopped the music.".to_string(),
            },
        })
    }
}

impl ToolEmbedding for MusicControlTool {
    type InitError = Infallible;
    type Context = ();
    type State = ();

    /// Retrieval is by similarity to what the user actually said, so these are
    /// phrasings rather than documentation. A tool that does not rank for
    /// "change the song" never reaches the model.
    fn embedding_docs(&self) -> Vec<String> {
        vec![
            "Control music playback: play a song, artist or album, skip to the next \
             track, go back, pause, resume or stop the music."
                .to_string(),
            "play music".to_string(),
            "play a song by an artist".to_string(),
            "change the song".to_string(),
            "next song, skip this track".to_string(),
            "go back to the previous song".to_string(),
            "pause the music, resume the music, stop the music".to_string(),
            "put something else on".to_string(),
        ]
    }

    fn context(&self) -> Self::Context {}

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Self)
    }
}
