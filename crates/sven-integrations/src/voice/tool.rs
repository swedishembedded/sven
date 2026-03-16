// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `voice` tool — TTS synthesis, STT transcription, and voice calls.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use super::{AudioBuffer, CallParams, SttProvider, TtsProvider, VoiceCallProvider};

/// Tool providing the agent with voice capabilities.
///
/// # Actions
///
/// - `synthesize` — convert text to speech (returns base64 audio)
/// - `transcribe` — convert audio file to text
/// - `call` — place an outbound voice call
pub struct VoiceTool {
    tts: Option<Arc<dyn TtsProvider>>,
    stt: Option<Arc<dyn SttProvider>>,
    calls: Option<Arc<dyn VoiceCallProvider>>,
}

impl VoiceTool {
    pub fn new(
        tts: Option<Arc<dyn TtsProvider>>,
        stt: Option<Arc<dyn SttProvider>>,
        calls: Option<Arc<dyn VoiceCallProvider>>,
    ) -> Self {
        Self { tts, stt, calls }
    }
}

#[async_trait]
impl Tool for VoiceTool {
    fn name(&self) -> &str {
        "voice"
    }

    fn description(&self) -> &str {
        "Voice capabilities: synthesize speech from text, transcribe audio to text, \
         or place an outbound voice call.\n\
         Actions: synthesize | transcribe | call\n\
         Use call to confirm appointments, collect notes, or deliver verbal messages. \
         Use synthesize to create audio files. Use transcribe to process recorded audio."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["synthesize", "transcribe", "call"],
                    "description": "Voice operation to perform"
                },
                "text": {
                    "type": "string",
                    "description": "(synthesize/call) Text to speak"
                },
                "voice": {
                    "type": "string",
                    "description": "(synthesize/call) Voice ID or name. Uses configured default if absent."
                },
                "audio_path": {
                    "type": "string",
                    "description": "(transcribe) Path to audio file (MP3, WAV, M4A, etc.)"
                },
                "to": {
                    "type": "string",
                    "description": "(call) Phone number to call in E.164 format, e.g. +12065551234"
                },
                "script": {
                    "type": "string",
                    "description": "(call) Message to speak when the call is answered"
                },
                "output_path": {
                    "type": "string",
                    "description": "(synthesize) File path to save the audio output"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing 'action'"),
        };

        match action {
            "synthesize" => self.synthesize(call).await,
            "transcribe" => self.transcribe(call).await,
            "call" => self.place_call(call).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; expected synthesize|transcribe|call"),
            ),
        }
    }
}

impl VoiceTool {
    async fn synthesize(&self, call: &ToolCall) -> ToolOutput {
        let tts = match &self.tts {
            Some(t) => t.clone(),
            None => return ToolOutput::err(&call.id, "TTS provider not configured"),
        };

        let text = match call.args.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err(&call.id, "synthesize requires 'text'"),
        };
        let voice = call
            .args
            .get("voice")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let output_path = call
            .args
            .get("output_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match tts.synthesize(&text, voice.as_deref()).await {
            Ok(audio) => {
                if let Some(path) = output_path {
                    match tokio::fs::write(&path, &audio.bytes).await {
                        Ok(()) => ToolOutput::ok(
                            &call.id,
                            format!(
                                "Audio saved to {path} ({} bytes, {}).",
                                audio.bytes.len(),
                                audio.mime_type
                            ),
                        ),
                        Err(e) => ToolOutput::err(&call.id, format!("failed to save audio: {e}")),
                    }
                } else {
                    ToolOutput::ok(
                        &call.id,
                        format!(
                            "Synthesized {} bytes of audio ({}). \
                             Use 'output_path' to save to a file.",
                            audio.bytes.len(),
                            audio.mime_type
                        ),
                    )
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("synthesize failed: {e}")),
        }
    }

    async fn transcribe(&self, call: &ToolCall) -> ToolOutput {
        let stt = match &self.stt {
            Some(s) => s.clone(),
            None => return ToolOutput::err(&call.id, "STT provider not configured"),
        };

        let path = match call.args.get("audio_path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "transcribe requires 'audio_path'"),
        };

        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(&call.id, format!("failed to read audio file: {e}")),
        };

        let mime = guess_mime(&path);
        let audio = AudioBuffer {
            bytes,
            mime_type: mime,
        };

        match stt.transcribe(&audio).await {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, format!("transcribe failed: {e}")),
        }
    }

    async fn place_call(&self, call: &ToolCall) -> ToolOutput {
        let calls_provider = match &self.calls {
            Some(c) => c.clone(),
            None => return ToolOutput::err(&call.id, "voice call provider not configured"),
        };

        let to = match call.args.get("to").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err(&call.id, "call requires 'to'"),
        };
        let script = call
            .args
            .get("script")
            .and_then(|v| v.as_str())
            .or_else(|| call.args.get("text").and_then(|v| v.as_str()))
            .unwrap_or("Hello, this is sven. Have a great day!")
            .to_string();
        let voice = call
            .args
            .get("voice")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let params = CallParams {
            to,
            script,
            voice,
            timeout_secs: None,
        };

        match calls_provider.call(&params).await {
            Ok(summary) => ToolOutput::ok(
                &call.id,
                format!(
                    "Call to {} initiated: ID={}, status={}",
                    summary.to, summary.call_id, summary.status
                ),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("call failed: {e}")),
        }
    }
}

fn guess_mime(path: &str) -> String {
    if path.ends_with(".mp3") {
        "audio/mpeg".to_string()
    } else if path.ends_with(".wav") {
        "audio/wav".to_string()
    } else if path.ends_with(".m4a") || path.ends_with(".mp4") {
        "audio/mp4".to_string()
    } else if path.ends_with(".ogg") {
        "audio/ogg".to_string()
    } else {
        "audio/mpeg".to_string()
    }
}

impl ToolDisplay for VoiceTool {
    fn display_name(&self) -> &str {
        "Voice"
    }
    fn icon(&self) -> &str {
        "🎙️"
    }
    fn category(&self) -> &str {
        "integrations"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        if action == "call" {
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            format!("call {to}")
        } else {
            action.to_string()
        }
    }
}
