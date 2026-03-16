// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Voice integration — TTS, STT, and voice calls.
//!
//! # Providers
//!
//! | Role | Provider | Notes |
//! |------|----------|-------|
//! | TTS | [`ElevenLabsTts`] | High-quality, multilingual |
//! | TTS | [`OpenAiTts`] | Fast, integrated with OpenAI |
//! | STT | [`WhisperStt`] | OpenAI Whisper via REST API |
//! | Calls | [`TwilioCallProvider`] | Outbound calls with TwiML |

pub mod elevenlabs;
pub mod openai_stt;
pub mod tool;
pub mod twilio;
pub mod types;

pub use elevenlabs::ElevenLabsTts;
pub use openai_stt::WhisperStt;
pub use tool::VoiceTool;
pub use twilio::TwilioCallProvider;
pub use types::{AudioBuffer, CallParams, CallSummary};

use async_trait::async_trait;

/// Text-to-speech provider trait.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize `text` into audio.
    ///
    /// Returns raw audio bytes in MP3 format.
    async fn synthesize(&self, text: &str, voice: Option<&str>) -> anyhow::Result<AudioBuffer>;
}

/// Speech-to-text provider trait.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe audio bytes to text.
    async fn transcribe(&self, audio: &AudioBuffer) -> anyhow::Result<String>;
}

/// Voice call provider trait.
#[async_trait]
pub trait VoiceCallProvider: Send + Sync {
    /// Initiate an outbound voice call.
    ///
    /// The call plays TTS of `params.script` when the callee answers.
    /// Returns a [`CallSummary`] after the call completes (or is attempted).
    async fn call(&self, params: &CallParams) -> anyhow::Result<CallSummary>;
}
