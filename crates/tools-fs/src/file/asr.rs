// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Speech-to-text used when the active model cannot accept audio natively.
//!
//! The clip is handed to an **already-running** brain server over its generic
//! capability surface: one `Run` of the `transcribe` action against the
//! configured model id.
//!
//! # Why not a subprocess
//!
//! This used to shell out per clip. That pays a process spawn, a device
//! handshake and a full checkpoint load every single time -- for the ASR model
//! this repo uses, 2.4 GiB from disk onto the GPU before a single sample is
//! examined, to transcribe two seconds of speech. A resident server loads once
//! and every later call is the inference alone.
//!
//! It also removes a second, easily-skewed spelling of the model. brain's CLI
//! dispatches on a bare architecture id (`nemotronasr`) while the served
//! surface uses the prefixed manifest id (`brain/nemotronasr`); the shell-out
//! path had drifted onto `brain/qwen-asr`, which neither accepts.
//!
//! # Wire format
//!
//! brain does no format sniffing: whatever bytes arrive on the fd reach the
//! model as-is. Audio is therefore sent as headerless mono `f32`
//! little-endian PCM at [`ASR_SAMPLE_RATE`], described by
//! `{"media":"audio","sample_rate":16000}` -- exactly what brain's audio
//! actions declare. Handing over a WAV container unchanged would feed its
//! 44-byte RIFF header to the model as samples.

use std::collections::HashMap;
use std::path::Path;

use crate::AsrConfig;
use sven_model_drivers::dbus::{ActionClient, ActionInput};
use thiserror::Error;
use tracing::debug;

/// Sample rate the ASR model expects. brain's `transcribe` declares
/// `sample_rate` with a hard requirement of 16 kHz.
pub const ASR_SAMPLE_RATE: u32 = 16_000;

/// The action every ASR model in brain exposes.
const TRANSCRIBE_ACTION: &str = "transcribe";

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("could not decode audio for transcription: {0}")]
    Decode(#[from] sven_audio::AudioError),

    #[error("transcription timed out after {timeout_secs}s (model {model})")]
    Timeout { model: String, timeout_secs: u64 },

    #[error("could not reach brain for transcription (model {model}): {source:#}")]
    Unreachable {
        model: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("transcription of {model} returned no `text` output: {detail}")]
    NoText { model: String, detail: String },
}

/// A completed transcription.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// The recognised text.
    pub text: String,
    /// Duration of the source audio in seconds.
    pub duration_secs: f32,
}

/// Params for one `transcribe` call.
///
/// `sample_rate` is sent explicitly rather than left to the action's default:
/// the audio was resampled to a rate this side chose, so stating it keeps the
/// two halves from disagreeing silently if either default ever moves.
pub(crate) fn transcribe_params(sample_rate: u32) -> serde_json::Value {
    serde_json::json!({ "sample_rate": sample_rate })
}

/// Transcribe `path` against the configured brain server.
pub async fn transcribe(path: &Path, cfg: &AsrConfig) -> Result<Transcript, AsrError> {
    transcribe_with(path, cfg, ActionClient::new(cfg.bus_address.as_deref())).await
}

/// [`transcribe`], against a caller-supplied client.
///
/// The seam a test uses: a client built on a peer-to-peer connection reaches a
/// fake `Brain1.Manager` with no bus daemon and no real model involved.
pub async fn transcribe_with(
    path: &Path,
    cfg: &AsrConfig,
    client: ActionClient,
) -> Result<Transcript, AsrError> {
    let pcm = sven_audio::load_pcm_at(path, ASR_SAMPLE_RATE)?;
    let duration_secs = pcm.duration_secs();
    let bytes = sven_audio::to_f32_le_bytes(&pcm.samples);

    debug!(
        model = %cfg.model,
        bus = cfg.bus_address.as_deref().unwrap_or("session"),
        samples = pcm.samples.len(),
        "transcribing over the brain capability surface"
    );

    let mut inputs = HashMap::new();
    inputs.insert(
        "audio".to_string(),
        ActionInput::audio_pcm_f32(bytes, ASR_SAMPLE_RATE),
    );

    let timeout = std::time::Duration::from_secs(cfg.timeout_secs.max(1));
    let params = transcribe_params(ASR_SAMPLE_RATE);
    let call = client.run(&cfg.model, TRANSCRIBE_ACTION, &params, &inputs);
    let outcome = match tokio::time::timeout(timeout, call).await {
        Err(_) => {
            return Err(AsrError::Timeout {
                model: cfg.model.clone(),
                timeout_secs: cfg.timeout_secs,
            })
        }
        Ok(Err(source)) => {
            return Err(AsrError::Unreachable {
                model: cfg.model.clone(),
                source,
            })
        }
        Ok(Ok(outcome)) => outcome,
    };

    let text = outcome.text("text").ok_or_else(|| AsrError::NoText {
        model: cfg.model.clone(),
        detail: format!("outputs: {:.256}", outcome.outputs),
    })?;

    Ok(Transcript {
        text: text.trim().to_string(),
        duration_secs,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// brain's `transcribe` requires 16 kHz and says so in its own param help.
    /// Sending audio resampled to one rate while declaring another is a silent
    /// corruption, not an error, so the two must come from one constant.
    #[test]
    fn params_declare_the_rate_the_audio_was_resampled_to() {
        let p = transcribe_params(ASR_SAMPLE_RATE);
        assert_eq!(p["sample_rate"], 16_000);
        assert_eq!(ASR_SAMPLE_RATE, 16_000);
    }

    /// The served surface reports prefixed manifest ids; brain's CLI dispatches
    /// on bare architecture ids and rejects the prefixed spelling. Now that the
    /// call goes over the served surface, the default must be the former.
    #[test]
    fn the_default_model_is_the_served_manifest_id() {
        let model = AsrConfig::default().model;
        assert!(
            model.starts_with("brain/"),
            "expected a served manifest id, got {model}"
        );
    }

    /// A missing `text` output must be reported as such, naming what did come
    /// back -- an empty reply and a reply in an unexpected shape are different
    /// failures and a caller cannot act on them the same way.
    #[test]
    fn a_reply_without_text_is_a_named_error() {
        let outcome = sven_model_drivers::dbus::ActionOutcome {
            outputs: serde_json::json!({ "num_tokens": 0 }),
            blobs: HashMap::new(),
        };
        assert!(outcome.text("text").is_none());
    }

    /// Text may arrive inline in the result JSON or as a blob on an fd, and
    /// which one brain picks is a property of the action. Both must work, with
    /// the blob winning when present.
    #[test]
    fn text_is_read_from_either_the_blob_or_the_scalar() {
        let inline = sven_model_drivers::dbus::ActionOutcome {
            outputs: serde_json::json!({ "text": "from the scalar" }),
            blobs: HashMap::new(),
        };
        assert_eq!(inline.text("text").as_deref(), Some("from the scalar"));

        let mut blobs = HashMap::new();
        blobs.insert("text".to_string(), b"from the blob".to_vec());
        let blobbed = sven_model_drivers::dbus::ActionOutcome {
            outputs: serde_json::json!({ "text": "from the scalar" }),
            blobs,
        };
        assert_eq!(blobbed.text("text").as_deref(), Some("from the blob"));
    }
}
