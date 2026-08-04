// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OpenAI Whisper speech-to-text provider.
//!
//! Also provides an OpenAI TTS implementation.
//!
//! # Configuration
//! ```yaml
//! tools:
//!   voice:
//!     stt_provider: openai
//!     tts_provider: openai
//!     tts_api_key: "${OPENAI_API_KEY}"
//!     tts_voice_id: "alloy"   # alloy | echo | fable | onyx | nova | shimmer
//! ```

use async_trait::async_trait;
use tracing::debug;

use super::{AudioBuffer, SttProvider, TtsProvider};

/// OpenAI Whisper STT provider.
pub struct WhisperStt {
    api_key: String,
    client: reqwest::Client,
}

impl WhisperStt {
    /// Create a new Whisper STT provider.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("Whisper HTTP client"),
        }
    }
}

#[async_trait]
impl SttProvider for WhisperStt {
    async fn transcribe(&self, audio: &AudioBuffer) -> anyhow::Result<String> {
        debug!(bytes = audio.bytes.len(), "Whisper: transcribing");

        let part = reqwest::multipart::Part::bytes(audio.bytes.clone())
            .file_name("audio.mp3")
            .mime_str(&audio.mime_type)?;

        let form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .part("file", part);

        let resp = self
            .client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        resp["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Whisper: unexpected response: {resp}"))
    }
}

/// OpenAI TTS provider.
pub struct OpenAiTts {
    api_key: String,
    default_voice: String,
    client: reqwest::Client,
}

impl OpenAiTts {
    /// Create a new OpenAI TTS provider.
    pub fn new(api_key: impl Into<String>, default_voice: Option<String>) -> Self {
        Self {
            api_key: api_key.into(),
            default_voice: default_voice.unwrap_or_else(|| "alloy".to_string()),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("OpenAI TTS HTTP client"),
        }
    }
}

#[async_trait]
impl TtsProvider for OpenAiTts {
    async fn synthesize(&self, text: &str, voice: Option<&str>) -> anyhow::Result<AudioBuffer> {
        let voice_id = voice.unwrap_or(&self.default_voice);
        debug!(voice_id, chars = text.len(), "OpenAI TTS: synthesizing");

        let payload = serde_json::json!({
            "model": "tts-1",
            "input": text,
            "voice": voice_id,
            "response_format": "mp3"
        });

        let bytes = self
            .client
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        Ok(AudioBuffer::mp3(bytes.to_vec()))
    }
}
