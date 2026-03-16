// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Voice integration types.

use serde::{Deserialize, Serialize};

/// Raw audio buffer (bytes + format hint).
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// Raw audio bytes.
    pub bytes: Vec<u8>,
    /// MIME type / format (e.g. `"audio/mpeg"`, `"audio/wav"`).
    pub mime_type: String,
}

impl AudioBuffer {
    pub fn mp3(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            mime_type: "audio/mpeg".to_string(),
        }
    }

    pub fn wav(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            mime_type: "audio/wav".to_string(),
        }
    }
}

/// Parameters for initiating a voice call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallParams {
    /// Phone number to call in E.164 format (e.g. `+12065551234`).
    pub to: String,
    /// Script/message to speak when the call is answered.
    pub script: String,
    /// Voice ID or name to use for TTS on the call.
    pub voice: Option<String>,
    /// Timeout in seconds before treating the call as unanswered.
    pub timeout_secs: Option<u64>,
}

/// Summary of a completed (or failed) voice call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSummary {
    /// Provider-specific call SID / ID.
    pub call_id: String,
    /// Dialed number.
    pub to: String,
    /// Final call status (e.g. `"completed"`, `"no-answer"`, `"failed"`).
    pub status: String,
    /// Call duration in seconds (0 if not answered).
    pub duration_secs: u64,
    /// Transcription of the call, if available.
    pub transcript: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_buffer_mp3_sets_mime() {
        let buf = AudioBuffer::mp3(vec![1, 2, 3]);
        assert_eq!(buf.mime_type, "audio/mpeg");
        assert_eq!(buf.bytes, vec![1, 2, 3]);
    }

    #[test]
    fn audio_buffer_wav_sets_mime() {
        let buf = AudioBuffer::wav(vec![4, 5]);
        assert_eq!(buf.mime_type, "audio/wav");
    }

    #[test]
    fn call_params_roundtrips_json() {
        let params = CallParams {
            to: "+12065551234".to_string(),
            script: "Hello, this is a test call.".to_string(),
            voice: Some("nova".to_string()),
            timeout_secs: Some(30),
        };
        let json = serde_json::to_string(&params).unwrap();
        let decoded: CallParams = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.to, "+12065551234");
        assert_eq!(decoded.voice, Some("nova".to_string()));
    }

    #[test]
    fn call_summary_roundtrips_json() {
        let summary = CallSummary {
            call_id: "CA123".to_string(),
            to: "+12065551234".to_string(),
            status: "completed".to_string(),
            duration_secs: 42,
            transcript: Some("Hello, confirmed.".to_string()),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let decoded: CallSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.call_id, "CA123");
        assert_eq!(decoded.duration_secs, 42);
        assert!(decoded.transcript.is_some());
    }
}
