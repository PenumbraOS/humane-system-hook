use std::time::Instant;

use std::sync::Arc;

use base64::Engine as _;
use reqwest::Client as HttpClient;
use rig::agent::{Agent, AgentBuilder, HookAction, PromptHook};
use rig::client::CompletionClient;
use rig::completion::message::{AssistantContent, ImageMediaType, Message, UserContent};
use rig::completion::CompletionModel;
use rig::completion::Prompt;
use rig::completion::{CompletionResponse, PromptError};
use rig::tool::Tool;
use rig::OneOrMany;
use tracing::error;

use crate::config::ResolvedConfig;
use crate::llm::ChatResult;

use super::backend::{LlmBackend, LlmFuture};
use super::error::friendly_error_message;
use super::memory::MemoryService;
use super::prompt::PromptBuilder;
use super::request::LlmChatRequest;
use super::request_log::LlmRequestLogger;
use super::tools::control_music::ControlMusicTool;
use super::tools::play_music::PlayMusicTool;
use super::tools::registry::LlmToolContext;
use super::tools::understand_scene::UnderstandSceneTool;

/// Marker for a termination due to device vision request
const DEFERRED_VISION_SENTINEL: &str = "__HUMANE_DEFERRED_VISION__";
/// Marker for a termination due to a music-play request. The captured tool
/// arguments JSON is appended after this prefix.
const PLAY_MUSIC_SENTINEL: &str = "__HUMANE_PLAY_MUSIC__";

const MUSIC_CONTROL_SENTINEL: &str = "__HUMANE_MUSIC_CONTROL__";

/// Map the `control_music` tool's semantic `action` to the Humane device
/// transport action name. Returns `None` for an unrecognized action so the
/// agent falls back to a normal response.
fn music_control_action(args: &serde_json::Value) -> Option<&'static str> {
    match args.get("action").and_then(|a| a.as_str())?.trim() {
        "pause" | "stop" => Some("PauseMusic"),
        "resume" | "play" | "continue" => Some("ResumeMusic"),
        "next" | "skip" => Some("NextTrack"),
        "previous" | "back" | "prev" => Some("PreviousTrack"),
        "restart" => Some("RestartTrack"),
        "favorite" | "like" => Some("SaveCurrentTrackToFavorites"),
        "play_favorites" | "favorites" => Some("PlayFavoriteTracks"),
        "radio" | "station" => Some("PlayCurrentTrackRadio"),
        _ => None,
    }
}

/// Rig hook to prevent execution of the `understand_scene` tool. The returned termination value
/// is used to trigger a DeferredVision response to the client
#[derive(Clone)]
struct DeferredVisionHook;

impl<M> PromptHook<M> for DeferredVisionHook
where
    M: CompletionModel,
{
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        // Intercept the no-op device tools before rig tries to execute them,
        // terminating the agent loop so the Understand handler can emit the
        // corresponding device action. play_music also carries its args JSON.
        for content in response.choice.iter() {
            if let AssistantContent::ToolCall(call) = content {
                if call.function.name == UnderstandSceneTool::NAME {
                    return HookAction::terminate(DEFERRED_VISION_SENTINEL.to_string());
                }
                if call.function.name == PlayMusicTool::NAME {
                    let args = call.function.arguments.to_string();
                    return HookAction::terminate(format!("{PLAY_MUSIC_SENTINEL}{args}"));
                }
                if call.function.name == ControlMusicTool::NAME {
                    if let Some(action) = music_control_action(&call.function.arguments) {
                        return HookAction::terminate(format!("{MUSIC_CONTROL_SENTINEL}{action}"));
                    }
                }
            }
        }
        HookAction::cont()
    }
}

/// Shared LLM backend for providers
pub struct RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    provider_label: &'static str,
    agent: Agent<M>,
    request_logger: LlmRequestLogger,
    max_tool_turns: usize,
    tool_concurrency: usize,
}

impl<M> RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    pub async fn from_client<C, F>(
        provider_label: &'static str,
        client: C,
        request_logger: LlmRequestLogger,
        config: &ResolvedConfig,
        http_client: HttpClient,
        memory: Option<MemoryService>,
        customize_builder: F,
    ) -> Result<Arc<dyn LlmBackend>, Box<dyn std::error::Error + Send + Sync>>
    where
        C: CompletionClient<CompletionModel = M>,
        F: FnOnce(AgentBuilder<M>) -> AgentBuilder<M>,
    {
        let llm_config = &config.config.llm;
        // Anthropic requires max_tokens on every request; harmless for others.
        let builder = customize_builder(
            client
                .agent(&llm_config.model)
                .max_tokens(llm_config.max_tokens),
        );

        let tool_resources = if llm_config.tools.enabled {
            let tool_context = LlmToolContext::new(http_client, config, memory);
            tool_context
                .build_tool_resources(llm_config)
                .await
                .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> {
                    std::io::Error::new(std::io::ErrorKind::Other, err).into()
                })?
        } else {
            None
        };

        let agent = match tool_resources {
            Some(resources) => resources.apply(builder).build(),
            None => builder.build(),
        };

        Ok(Arc::new(Self {
            provider_label,
            agent,
            request_logger,
            max_tool_turns: llm_config.tools.max_tool_turns,
            tool_concurrency: llm_config.tools.tool_concurrency,
        }))
    }
}

impl<M> LlmBackend for RigBackend<M>
where
    M: CompletionModel + 'static,
    (): PromptHook<M> + 'static,
{
    fn chat<'a>(&'a self, request: LlmChatRequest) -> LlmFuture<'a> {
        Box::pin(async move {
            let utterance = request.utterance.clone();
            let run_id = request.template_context.run_id.clone();
            let history = PromptBuilder::build_chat_history(&request);
            let started = Instant::now();

            let content = if let Some(image_bytes) = &request.image {
                OneOrMany::many(vec![
                    UserContent::text(utterance.clone()),
                    UserContent::image_base64(
                        &base64::engine::general_purpose::STANDARD.encode(image_bytes),
                        Some(ImageMediaType::JPEG),
                        None,
                    ),
                ])
                .expect("non-empty content vec")
            } else {
                OneOrMany::one(UserContent::text(utterance.clone()))
            };

            let user_message = Message::User { content };

            let raw_result = self
                .agent
                .prompt(user_message)
                .with_history(history.clone())
                .max_turns(self.max_tool_turns)
                .with_tool_concurrency(self.tool_concurrency.max(1))
                .with_hook(DeferredVisionHook)
                .await;
            let latency_ms = started.elapsed().as_millis();

            let result = match raw_result {
                Ok(text) => Ok(ChatResult::Text(text)),
                Err(PromptError::PromptCancelled { reason, .. })
                    if reason == DEFERRED_VISION_SENTINEL =>
                {
                    Ok(ChatResult::DeferredVision)
                }
                Err(PromptError::PromptCancelled { reason, .. })
                    if reason.starts_with(PLAY_MUSIC_SENTINEL) =>
                {
                    Ok(ChatResult::PlayMusic(
                        reason[PLAY_MUSIC_SENTINEL.len()..].to_string(),
                    ))
                }
                Err(PromptError::PromptCancelled { reason, .. })
                    if reason.starts_with(MUSIC_CONTROL_SENTINEL) =>
                {
                    Ok(ChatResult::MusicControl(
                        reason[MUSIC_CONTROL_SENTINEL.len()..].to_string(),
                    ))
                }
                Err(e) => {
                    error!(provider = self.provider_label, error = %e, "LLM chat failed");
                    Err(friendly_error_message(&e))
                }
            };

            self.request_logger
                .log_chat(
                    self.provider_label,
                    &run_id,
                    &history,
                    &utterance,
                    match &result {
                        Ok(ChatResult::Text(text)) => Some(text.as_str()),
                        Ok(ChatResult::DeferredVision) => None,
                        Ok(ChatResult::PlayMusic(_)) => None,
                        Ok(ChatResult::MusicControl(_)) => None,
                        Err(_) => None,
                    },
                    result.clone().err().as_deref(),
                    latency_ms,
                )
                .await;

            result
        })
    }
}
