// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `attach_file` — attach an image or audio file to the conversation.
//!
//! Images become an image content part.  Audio takes one of two routes:
//! attached natively when the active model accepts audio, or transcribed to
//! plain text otherwise — so the result is always something the target model
//! can actually consume.
//!
//! Note that a model with no tool-calling support will never invoke this tool.
//! The `--attach <PATH>` CLI flag covers that case by pre-loading attachments
//! into the initial user turn; both paths share
//! [`attachment::load_attachment`] so they cannot drift.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_config::AsrConfig;
use sven_model::ModelProvider;
use tracing::debug;

use super::attachment::{self, AttachOptions, SUPPORTED_AUDIO_EXTS, SUPPORTED_IMAGE_EXTS};
use sven_tool_api::{ApprovalPolicy, PathScope, Tool, ToolCall, ToolOutput};

pub struct AttachFileTool {
    /// The live model, when one is available.
    ///
    /// `None` (e.g. the `sven tool call …` CLI registry) means "assume
    /// text-only": images are refused and audio is always transcribed, which
    /// is the only answer that is correct for every possible target model.
    model: Option<Arc<dyn ModelProvider>>,
    /// Where transcription is sent for the audio fallback.
    asr: AsrConfig,
    /// A pre-built transcription client, for tests. `None` in production, where
    /// one is dialled from `asr`.
    asr_client: Option<sven_model::ActionClient>,
    /// Where `path` resolves, and whether it may leave there.
    scope: PathScope,
}

impl AttachFileTool {
    pub fn new(model: Option<Arc<dyn ModelProvider>>, asr: AsrConfig) -> Self {
        Self {
            model,
            asr,
            asr_client: None,
            scope: PathScope::default(),
        }
    }

    /// Resolves `path` through `scope` instead of the process working
    /// directory.
    #[must_use]
    pub fn with_scope(mut self, scope: PathScope) -> Self {
        self.scope = scope;
        self
    }

    /// Use `client` for transcription instead of dialling one from `asr`.
    ///
    /// Exists for tests: a client wired to a fake `Brain1.Manager` exercises
    /// the audio path with no bus, no server and no checkpoint.
    #[cfg(test)]
    pub(crate) fn with_asr_client(mut self, client: sven_model::ActionClient) -> Self {
        self.asr_client = Some(client);
        self
    }

    /// Capabilities of the target model, conservative when none is known.
    fn options(&self, force_transcribe: bool) -> AttachOptions {
        AttachOptions {
            supports_images: self.model.as_ref().is_some_and(|m| m.supports_images()),
            supports_audio: self.model.as_ref().is_some_and(|m| m.supports_audio()),
            force_transcribe,
            asr: self.asr.clone(),
            asr_client: self.asr_client.clone(),
        }
    }

    fn model_label(&self) -> String {
        match &self.model {
            Some(m) => format!("{}/{}", m.name(), m.model_name()),
            None => "unknown (no live model in this context)".to_string(),
        }
    }
}

#[async_trait]
impl Tool for AttachFileTool {
    fn name(&self) -> &str {
        "attach_file"
    }

    fn description(&self) -> &str {
        "Attach an image or audio file to the conversation so the model can perceive it.\n\
         Images are sent as image content; audio is sent as audio when the model supports it, \
         otherwise transcribed to text automatically.\n\
         Supports images (png/jpg/gif/webp/bmp/tiff) and audio (wav; mp3/flac/ogg/m4a are \
         recognised but need an external decoder)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description":
                        "Path to an image (png/jpg/gif/webp/bmp/tiff) or audio \
                         (wav/mp3/flac/ogg/m4a) file"
                },
                "note": {
                    "type": "string",
                    "description": "Optional note recorded alongside the attachment"
                },
                "force_transcribe": {
                    "type": "boolean",
                    "description":
                        "Audio only: transcribe to text even when the model accepts audio natively",
                    "default": false
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path_str = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            Some(_) => {
                return ToolOutput::err(&call.id, "parameter 'path' must not be empty");
            }
            None => {
                let args_preview =
                    serde_json::to_string(&call.args).unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'path'. Received: {args_preview}"),
                );
            }
        };

        let force_transcribe = call
            .args
            .get("force_transcribe")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let note = call
            .args
            .get("note")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let scoped = match self.scope.resolve_for(call, &path_str) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let path = scoped.as_path();
        if attachment::classify(path).is_none() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            return ToolOutput::err(
                &call.id,
                format!(
                    "file does not appear to be an image or audio file (extension: .{ext}). \
                     Supported images: {SUPPORTED_IMAGE_EXTS}. Supported audio: {SUPPORTED_AUDIO_EXTS}."
                ),
            );
        }

        debug!(path = %path_str, force_transcribe, "attach_file tool");

        let opts = self.options(force_transcribe);
        match attachment::load_attachment(path, &opts, &self.model_label()).await {
            Ok(loaded) => {
                let mut parts = loaded.into_tool_parts();
                if let Some(n) = note {
                    parts.insert(0, sven_tool_api::ToolOutputPart::Text(format!("Note: {n}")));
                }
                ToolOutput::with_parts(&call.id, parts)
            }
            // `{:#}` renders the whole error chain, so the ASR command name and
            // its stderr tail reach the model and it can react.
            Err(e) => ToolOutput::err(&call.id, format!("attach_file failed: {e:#}")),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_model_mock::ScriptedMockProvider;

    use super::super::attachment::tests_support::*;
    use super::*;
    use sven_tool_api::ToolOutputPart;

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "af1".into(),
            name: "attach_file".into(),
            args,
        }
    }

    /// A model that reports vision but no audio.
    fn vision_model() -> Arc<dyn ModelProvider> {
        Arc::new(ScriptedMockProvider::new(vec![]).with_vision())
    }

    /// A model that reports both vision and native audio.
    fn omni_model() -> Arc<dyn ModelProvider> {
        Arc::new(ScriptedMockProvider::new(vec![]).with_vision().with_audio())
    }

    /// A text-only model.
    fn text_model() -> Arc<dyn ModelProvider> {
        Arc::new(ScriptedMockProvider::new(vec![]))
    }

    fn asr_cfg() -> AsrConfig {
        AsrConfig {
            model: "brain/nemotronasr".into(),
            bus_address: None,
            timeout_secs: 30,
        }
    }

    // ── Image ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn image_produces_an_image_part() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.png");
        std::fs::write(&png, MINIMAL_PNG).unwrap();

        let t = AttachFileTool::new(Some(vision_model()), AsrConfig::default());
        let out = t
            .execute(&call(json!({"path": png.display().to_string()})))
            .await;

        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(out.has_images(), "should carry an image part");
        assert!(out.content.contains("Attached image"), "{}", out.content);
        assert!(matches!(out.parts[1], ToolOutputPart::Image(_)));
    }

    #[tokio::test]
    async fn image_rejected_for_text_only_model() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.png");
        std::fs::write(&png, MINIMAL_PNG).unwrap();

        let t = AttachFileTool::new(Some(text_model()), AsrConfig::default());
        let out = t
            .execute(&call(json!({"path": png.display().to_string()})))
            .await;

        assert!(out.is_error, "text-only model must refuse images");
        assert!(
            out.content.contains("does not accept image input"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn note_is_prepended_to_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.png");
        std::fs::write(&png, MINIMAL_PNG).unwrap();

        let t = AttachFileTool::new(Some(vision_model()), AsrConfig::default());
        let out = t
            .execute(&call(
                json!({"path": png.display().to_string(), "note": "the failing screen"}),
            ))
            .await;

        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.starts_with("Note: the failing screen"),
            "{}",
            out.content
        );
    }

    // ── Audio: native ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn audio_attaches_natively_when_model_supports_it() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("say.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();

        // An ASR endpoint nothing is listening on: if the tool tried to
        // transcribe at all, the call would fail loudly. Reaching a successful
        // native attachment proves it never went near the ASR path.
        let asr = AsrConfig {
            model: "brain/nemotronasr".into(),
            bus_address: Some("unix:path=/nonexistent/should-never-be-dialled".into()),
            timeout_secs: 5,
        };
        let t = AttachFileTool::new(Some(omni_model()), asr);
        let out = t
            .execute(&call(json!({"path": wav.display().to_string()})))
            .await;

        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(out.has_audio(), "should carry an audio part");
        assert!(!out.has_images());
        assert!(out.content.contains("Attached audio"), "{}", out.content);
        match &out.parts[1] {
            ToolOutputPart::Audio(url) => {
                assert!(url.starts_with("data:audio/wav;base64,"), "{url:.40}")
            }
            other => panic!("expected an audio part, got {other:?}"),
        }
    }

    // ── Audio: transcription fallback ─────────────────────────────────────────

    #[tokio::test]
    async fn audio_is_transcribed_when_model_lacks_audio_support() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("say.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let (client, _server) = fake_asr("put a box around the dog").await;

        let t = AttachFileTool::new(Some(text_model()), asr_cfg()).with_asr_client(client);
        let out = t
            .execute(&call(json!({"path": wav.display().to_string()})))
            .await;

        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(!out.has_audio(), "fallback must be text-only");
        assert!(out.content.contains("Transcript of"), "{}", out.content);
        assert!(
            out.content.contains("put a box around the dog"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn force_transcribe_bypasses_native_audio() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("say.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let (client, _server) = fake_asr("forced transcript").await;

        let t = AttachFileTool::new(Some(omni_model()), asr_cfg()).with_asr_client(client);
        let out = t
            .execute(&call(
                json!({"path": wav.display().to_string(), "force_transcribe": true}),
            ))
            .await;

        assert!(!out.is_error, "{}", out.content);
        assert!(!out.has_audio(), "force_transcribe must not attach audio");
        assert!(out.content.contains("forced transcript"), "{}", out.content);
    }

    #[tokio::test]
    async fn no_model_context_always_transcribes() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("say.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let (client, _server) = fake_asr("cli transcript").await;

        // model: None — the `sven tool call …` registry case.
        let t = AttachFileTool::new(None, asr_cfg()).with_asr_client(client);
        let out = t
            .execute(&call(json!({"path": wav.display().to_string()})))
            .await;

        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("cli transcript"), "{}", out.content);
    }

    // ── Error paths ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_path_argument_errors() {
        let t = AttachFileTool::new(None, AsrConfig::default());
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn empty_path_argument_errors() {
        let t = AttachFileTool::new(None, AsrConfig::default());
        let out = t.execute(&call(json!({"path": "   "}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("must not be empty"), "{}", out.content);
    }

    #[tokio::test]
    async fn unknown_extension_lists_supported_types() {
        let t = AttachFileTool::new(None, AsrConfig::default());
        let out = t.execute(&call(json!({"path": "main.rs"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("png"), "{}", out.content);
        assert!(out.content.contains("wav"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_image_file_errors() {
        let t = AttachFileTool::new(Some(vision_model()), AsrConfig::default());
        let out = t
            .execute(&call(json!({"path": "no_such_attach_xyz.png"})))
            .await;
        assert!(out.is_error);
        assert!(
            out.content.contains("attach_file failed"),
            "{}",
            out.content
        );
    }

    /// An unreachable server must name the model it could not transcribe with,
    /// not fail anonymously: the two ASR ids are easy to confuse and the bus
    /// may simply have no brain on it.
    #[tokio::test]
    async fn asr_failure_names_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("say.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        // An address nothing is listening on: the call cannot connect.
        let asr = AsrConfig {
            model: "brain/nemotronasr".into(),
            bus_address: Some(format!(
                "unix:path={}",
                dir.path().join("absent.sock").display()
            )),
            timeout_secs: 5,
        };

        let t = AttachFileTool::new(Some(text_model()), asr);
        let out = t
            .execute(&call(json!({"path": wav.display().to_string()})))
            .await;

        assert!(out.is_error);
        assert!(out.content.contains("brain/nemotronasr"), "{}", out.content);
    }

    #[tokio::test]
    async fn undecodable_audio_format_reports_no_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("say.mp3");
        std::fs::write(&mp3, b"ID3\x04\x00").unwrap();

        let t = AttachFileTool::new(Some(omni_model()), AsrConfig::default());
        let out = t
            .execute(&call(json!({"path": mp3.display().to_string()})))
            .await;

        assert!(out.is_error);
        assert!(
            out.content.contains("mp3 not supported: no decoder"),
            "{}",
            out.content
        );
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_declares_path_as_required() {
        let t = AttachFileTool::new(None, AsrConfig::default());
        let schema = t.parameters_schema();
        assert_eq!(schema["required"], json!(["path"]));
        assert!(schema["properties"]["note"].is_object());
        assert!(schema["properties"]["force_transcribe"].is_object());
    }
}
