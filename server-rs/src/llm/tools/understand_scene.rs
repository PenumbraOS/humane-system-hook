use std::convert::Infallible;

use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolEmbedding};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub struct UnderstandSceneTool;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UnderstandSceneToolContext;

impl Tool for UnderstandSceneTool {
    const NAME: &'static str = "understand_scene";

    type Error = Infallible;
    type Args = serde_json::Value;
    type Output = serde_json::Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Capture a photo and analyze what's in view. Use when the user asks \
                what they're looking at, what's in front of them, what object they're pointing \
                at, or wants you to visually identify or describe something in their environment. \
                This will trigger the device camera to take a picture and send it to you for \
                analysis."
                .to_string(),
            parameters: json!(null),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Unreachable. This tool will never actually be called
        Ok(json!({}))
    }
}

impl ToolEmbedding for UnderstandSceneTool {
    type InitError = Infallible;
    type Context = UnderstandSceneToolContext;
    type State = ();

    fn embedding_docs(&self) -> Vec<String> {
        vec![
            "Ask the device camera to capture and analyze the current scene or view. Use when the user wants you to see what they're looking at, identify objects, read text in the environment, or visually inspect something.".to_string(),
            "Capture a photo for visual analysis of the user's surroundings, food, documents, faces, products, or anything in their field of view. Use for questions like 'what am I looking at', 'what's in front of me', 'describe this', 'what is that object'.".to_string(),
            "Take a picture and analyze it to answer visual questions about the user's environment. Good for identifying items, reading signs, describing scenes, analyzing food, or examining products.".to_string(),
        ]
    }

    fn context(&self) -> Self::Context {
        UnderstandSceneToolContext
    }

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Self)
    }
}
