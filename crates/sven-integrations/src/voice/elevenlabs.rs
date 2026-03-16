// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! ElevenLabs text-to-speech provider.
//!
//! # Configuration
//! ```yaml
//! tools:
//!   voice:
//!     tts_provider: elevenlabs
//!     tts_api_key: "${ELEVENLABS_API_KEY}"
//!     tts_voice_id: "21m00Tcm4TlvDq8ikWAM"  # Rachel voice
//! ```

use async_trait::async_trait;
use tracing::debug;

use super::{AudioBuffer, TtsProvider};

const ELEVENLABS_API: &str = "https://api.elevenlabs.io/v1";

/// ElevenLabs TTS provider.
pub struct ElevenLabsTts {
    api_key: String,
    default_voice_id: String,
    client: reqwest::Client,
}

impl ElevenLabsTts {
    /// Default voice ID — Rachel (clear, English, US).
    pub const DEFAULT_VOICE: &'static str = "21m00Tcm4TlvDq8ikWAM";

    /// Create a new ElevenLabs TTS provider.
    pub fn new(api_key: impl Into<String>, default_voice_id: Option<String>) -> Self {
        Self {
            api_key: api_key.into(),
            default_voice_id: default_voice_id.unwrap_or_else(|| Self::DEFAULT_VOICE.to_string()),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("ElevenLabs HTTP client"),
        }
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsTts {
    async fn synthesize(&self, text: &str, voice: Option<&str>) -> anyhow::Result<AudioBuffer> {
        let voice_id = voice.unwrap_or(&self.default_voice_id);
        debug!(voice_id, chars = text.len(), "ElevenLabs: synthesizing");

        let payload = serde_json::json!({
            "text": text,
            "model_id": "eleven_monolingual_v1",
            "voice_settings": {
                "stability": 0.5,
                "similarity_boost": 0.75
            }
        });

        let bytes = self
            .client
            .post(format!("{ELEVENLABS_API}/text-to-speech/{voice_id}"))
            .header("xi-api-key", &self.api_key)
            .header("Accept", "audio/mpeg")
            .json(&payload)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        Ok(AudioBuffer::mp3(bytes.to_vec()))
    }
}
