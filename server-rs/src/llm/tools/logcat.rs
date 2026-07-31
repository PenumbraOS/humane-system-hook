use std::convert::Infallible;

use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolEmbedding};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tokio::process::Command;

use crate::util::truncate_on_char_boundary;

/// Tool that captures the full Android logcat buffer and writes it to
/// `/sdcard/PenumbraOS/logcat/logcat_<timestamp>[_<annotation>].txt`.
#[derive(Debug, Clone)]
pub struct DumpLogcatTool;

#[derive(Debug, Deserialize)]
pub struct DumpLogcatArgs {
    /// Optional context string describing why this dump was taken
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DumpLogcatResult {
    pub result: String,
}

#[derive(Debug)]
pub struct DumpLogcatError(String);

impl std::fmt::Display for DumpLogcatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DumpLogcatError {}

impl Tool for DumpLogcatTool {
    const NAME: &'static str = "dump_logcat";

    type Error = DumpLogcatError;
    type Args = DumpLogcatArgs;
    type Output = DumpLogcatResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Capture the full Android logcat (system log) buffer and write it to a file \
                 on disk. Use this ONLY when the user specifically requests logcat logs."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "annotation": {
                        "type": "string",
                        "description": "Optional short context string describing why this logcat \
                         dump was taken. Will be included in the filename for identification."
                    }
                },
                "required": []
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let output = Command::new("logcat")
            .args(["-d"])
            .output()
            .await
            .map_err(|e| DumpLogcatError(format!("failed to spawn logcat: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DumpLogcatError(format!("logcat exited non-zero: {stderr}")));
        }

        let stdout = output.stdout;

        let now = chrono::Local::now();
        let timestamp = now.format("%Y%m%d_%H%M%S").to_string();

        let filename = if let Some(ref annotation) = args.annotation {
            let sanitized = sanitize_annotation(annotation);
            if sanitized.is_empty() {
                format!("logcat_{timestamp}.log")
            } else {
                format!("logcat_{timestamp}_{sanitized}.log")
            }
        } else {
            format!("logcat_{timestamp}.log")
        };

        let log_dir = Path::new("/sdcard/PenumbraOS/logcat");
        tokio::fs::create_dir_all(log_dir)
            .await
            .map_err(|e| DumpLogcatError(format!("failed to create log directory: {e}")))?;

        let path = log_dir.join(&filename);

        // Write the logcat output to file
        tokio::fs::write(&path, &stdout)
            .await
            .map_err(|e| DumpLogcatError(format!("failed to write logcat dump: {e}")))?;

        Ok(DumpLogcatResult {
            result: "Logcat dump completed successfully".to_string(),
        })
    }
}

impl ToolEmbedding for DumpLogcatTool {
    type InitError = Infallible;
    type Context = ();
    type State = ();

    fn embedding_docs(&self) -> Vec<String> {
        vec![
            "Capture the full Android logcat (system log) buffer and write it to a file \
                 on disk. Use this ONLY when the user specifically requests logcat logs."
                .to_string(),
            "logcat dump".to_string(),
        ]
    }

    fn context(&self) -> Self::Context {
        ()
    }

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Self)
    }
}

/// Sanitize an annotation string for use as part of a filename.
fn sanitize_annotation(annotation: &str) -> String {
    let sanitized: String = annotation
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Replace spaces with underscores and collapse consecutive underscores
    let mut result = String::with_capacity(sanitized.len());
    let mut prev_underscore = false;
    for c in sanitized.chars() {
        let ch = if c == ' ' { '_' } else { c };
        if ch == '_' {
            if !prev_underscore {
                result.push('_');
                prev_underscore = true;
            }
        } else {
            result.push(ch);
            prev_underscore = false;
        }
    }

    // Trim trailing underscores
    let mut result = result.trim_end_matches('_').to_string();

    // The filter above keeps any `char::is_alphanumeric()` char, including
    // non-ASCII letters, so a plain `result[..30]` byte slice can split one.
    truncate_on_char_boundary(&mut result, 30);
    result
}
