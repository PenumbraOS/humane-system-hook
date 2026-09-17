//! The text-to-speech provider seam.
//!
//! The Pin speaks through Android's TTS framework, and the PenumbraOS hook owns
//! `HumaneTTSService.onSynthesizeText` — it already feeds raw PCM chunks to the
//! synthesis callback. The device's embedded Microsoft engine (`backend=offline`
//! in its own logs) is what makes the assistant sound robotic, so the hook asks
//! this server for audio instead and falls back to the embedded voice when the
//! server cannot answer.
//!
//! Everything downstream is fixed by what the hook hands Android:
//! **24 kHz, 16-bit signed little-endian, mono PCM**. Providers must emit exactly
//! that, with no container and no resampling — which both OpenAI (`response_format:
//! "pcm"`) and ElevenLabs (`output_format: "pcm_24000"`) produce natively.

mod openai;

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::config::Config;

/// The one audio format the device accepts. Not negotiable: the hook passes
/// these values straight to `SynthesisCallback.start()`.
pub const SAMPLE_RATE_HZ: u32 = 24_000;
pub const CHANNELS: u16 = 1;

/// Raw PCM, streamed so speech can start before synthesis finishes.
pub type PcmStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// The interface every voice backend implements.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Stable short name for logging.
    fn name(&self) -> &'static str;

    /// Speak `text` as 24 kHz 16-bit mono PCM.
    async fn synthesize(&self, text: &str) -> Result<PcmStream, String>;
}

pub type SharedTts = Arc<dyn TtsProvider>;

/// Build the configured provider, or `None` when TTS is disabled or unconfigured
/// — in which case the device simply keeps using its embedded voice.
pub fn from_config(config: &Config, http: reqwest::Client) -> Option<SharedTts> {
    if !config.tts.enabled {
        tracing::info!("TTS provider disabled by config");
        return None;
    }

    match config.tts.provider.as_str() {
        "openai" => match openai::OpenAiTts::from_config(config, http) {
            Ok(provider) => {
                tracing::info!(voice = %config.tts.voice, model = %config.tts.model, "OpenAI TTS ready");
                Some(Arc::new(provider))
            }
            Err(error) => {
                tracing::warn!(%error, "OpenAI TTS unavailable; keeping the device voice");
                None
            }
        },
        other => {
            tracing::warn!(provider = other, "unknown TTS provider; keeping the device voice");
            None
        }
    }
}
