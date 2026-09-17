//! OpenAI speech provider.
//!
//! `response_format: "pcm"` returns 24 kHz 16-bit mono PCM — exactly what the
//! device expects — so the bytes pass through untouched, and the response is
//! streamed so the Pin starts speaking before synthesis has finished.

use async_trait::async_trait;
use futures::TryStreamExt;
use reqwest::Client;
use serde_json::json;

use super::{PcmStream, TtsProvider};
use crate::config::Config;

const ENDPOINT: &str = "https://api.openai.com/v1/audio/speech";

pub struct OpenAiTts {
    api_key: String,
    model: String,
    voice: String,
    instructions: Option<String>,
    http: Client,
}

impl OpenAiTts {
    pub fn from_config(config: &Config, http: Client) -> Result<Self, String> {
        let api_key = config
            .tts
            .resolve_api_key()
            .ok_or("OpenAI TTS api_key not set; configure OPENAI_API_KEY or tts.api_key")?;

        Ok(Self {
            api_key,
            model: config.tts.model.clone(),
            voice: config.tts.voice.clone(),
            instructions: config
                .tts
                .instructions
                .clone()
                .filter(|value| !value.trim().is_empty()),
            http,
        })
    }
}

#[async_trait]
impl TtsProvider for OpenAiTts {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn synthesize(&self, text: &str) -> Result<PcmStream, String> {
        let mut body = json!({
            "model": self.model,
            "voice": self.voice,
            "input": text,
            // Raw PCM: 24kHz, 16-bit signed LE, mono. No container to strip.
            "response_format": "pcm",
        });
        // `instructions` steers delivery (tone, pace) on the 4o-mini-tts models.
        if let Some(instructions) = &self.instructions {
            body["instructions"] = json!(instructions);
        }

        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("openai tts request failed: {error}"))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            // Truncated: this reaches the logs on every failed utterance.
            let detail: String = detail.chars().take(300).collect();
            return Err(format!("openai tts returned {status}: {detail}"));
        }

        let stream = response
            .bytes_stream()
            .map_err(|error| std::io::Error::other(error.to_string()));

        Ok(Box::pin(stream))
    }
}
