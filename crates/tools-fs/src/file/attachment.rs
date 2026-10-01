// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Turning a file path into a multimodal content part.
//!
//! This is the single implementation of "classify a path, load it the right
//! way".  Both entry points use it, so the two paths cannot drift:
//!
//! * the `attach_file` **tool**, invoked by a tool-calling model, and
//! * the `--attach <PATH>` **CLI flag**, which pre-loads attachments into the
//!   initial user turn without needing any tool call.
//!
//! Audio takes one of two routes depending on what the target model accepts:
//! attached natively as a data URL, or transcribed to plain text so it flows
//! through every provider unchanged. Transcription is compiled in with the
//! `asr` feature (Unix only); without it, audio for a model that cannot hear is
//! refused with that reason.

use std::path::Path;

use sven_config::AsrConfig;
use sven_model::ContentPart;
use thiserror::Error;

#[cfg(all(unix, feature = "asr"))]
use super::asr::{self, AsrError};
use sven_tool_api::ToolOutputPart;

/// What kind of attachment a path names, based on its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Audio,
}

/// Extensions accepted for each kind, for use in error messages.
pub const SUPPORTED_IMAGE_EXTS: &str = "png, jpg, jpeg, gif, webp, bmp, tiff";
pub const SUPPORTED_AUDIO_EXTS: &str = "wav (mp3, m4a, flac, ogg are recognised but not decodable)";

/// Classify `path` by extension.  Returns `None` for anything unrecognised.
pub fn classify(path: &Path) -> Option<AttachmentKind> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if sven_image::is_image_extension(ext) {
        Some(AttachmentKind::Image)
    } else if sven_audio::is_audio_extension(ext) {
        Some(AttachmentKind::Audio)
    } else {
        None
    }
}

/// How to load an attachment for a particular target model.
///
/// The [`Default`] is deliberately conservative — no image support, no native
/// audio — because that combination always produces something every provider
/// can consume (a refusal for images, a transcript for audio).
#[derive(Debug, Clone, Default)]
pub struct AttachOptions {
    /// Whether the target model accepts image input.  When `false`, images are
    /// refused up front instead of being attached and silently stripped later.
    pub supports_images: bool,
    /// Whether the target model accepts audio input natively.  When `false`,
    /// audio is transcribed instead.
    pub supports_audio: bool,
    /// Transcribe audio even when the model would accept it natively.
    pub force_transcribe: bool,
    /// Where transcription is sent when audio must be turned into text.
    pub asr: AsrConfig,
    /// A pre-built action client for transcription, instead of one dialled
    /// from [`AttachOptions::asr`].
    ///
    /// `None` in every production path. A test supplies a client wired to a
    /// fake `Brain1.Manager` so the audio-to-transcript behaviour can be
    /// exercised without a bus, a server, or a 2.4 GiB checkpoint.
    #[cfg(all(unix, feature = "asr"))]
    pub asr_client: Option<sven_model_drivers::dbus::ActionClient>,
}

/// A file that has been loaded and is ready to be turned into content parts.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadedAttachment {
    /// An image, as a `data:image/…;base64,…` URL.
    Image { description: String, url: String },
    /// Audio attached natively, as a `data:audio/wav;base64,…` URL.
    Audio { description: String, url: String },
    /// Audio turned into text.  Carries no blob, so it survives every provider.
    Transcript { text: String },
}

impl LoadedAttachment {
    /// The plain-text line describing this attachment.
    pub fn text(&self) -> &str {
        match self {
            Self::Image { description, .. } | Self::Audio { description, .. } => description,
            Self::Transcript { text } => text,
        }
    }

    /// Render as tool output parts (for the `attach_file` tool).
    pub fn into_tool_parts(self) -> Vec<ToolOutputPart> {
        match self {
            Self::Image { description, url } => {
                vec![
                    ToolOutputPart::Text(description),
                    ToolOutputPart::Image(url),
                ]
            }
            Self::Audio { description, url } => {
                vec![
                    ToolOutputPart::Text(description),
                    ToolOutputPart::Audio(url),
                ]
            }
            Self::Transcript { text } => vec![ToolOutputPart::Text(text)],
        }
    }

    /// Render as message content parts (for the `--attach` CLI path).
    pub fn into_content_parts(self) -> Vec<ContentPart> {
        match self {
            Self::Image { description, url } => {
                vec![ContentPart::text(description), ContentPart::image(url)]
            }
            Self::Audio { description, url } => {
                vec![ContentPart::text(description), ContentPart::audio(url)]
            }
            Self::Transcript { text } => vec![ContentPart::text(text)],
        }
    }
}

#[derive(Debug, Error)]
pub enum AttachError {
    #[error(
        "unsupported file type (extension: .{ext}). \
         Supported images: {SUPPORTED_IMAGE_EXTS}. Supported audio: {SUPPORTED_AUDIO_EXTS}."
    )]
    UnknownExtension { ext: String },

    #[error(
        "the active model ({model}) does not accept image input, so attaching \
         '{path}' would have no effect. Describe the image in text, or switch \
         to a vision-capable model."
    )]
    ImagesUnsupported { model: String, path: String },

    #[error("failed to read image '{path}': {source}")]
    Image {
        path: String,
        #[source]
        source: sven_image::ImageError,
    },

    #[error("failed to read audio '{path}': {source}")]
    Audio {
        path: String,
        #[source]
        source: sven_audio::AudioError,
    },

    #[cfg(all(unix, feature = "asr"))]
    #[error("audio transcription failed for '{path}': {source}")]
    Asr {
        path: String,
        #[source]
        source: AsrError,
    },

    #[cfg(not(all(unix, feature = "asr")))]
    #[error(
        "the active model ({model}) does not accept audio, so '{path}' would have to \
         be transcribed, and this build has no speech-to-text (it needs a Unix target \
         and the `asr` feature). Describe the audio in text, or switch to a model \
         that accepts audio."
    )]
    TranscriptionUnavailable { model: String, path: String },
}

/// Classify `path`, load it, and describe it.
///
/// `model_label` is used only in the refusals (no image input, or audio that
/// cannot be transcribed), so the agent can see which model refused.
pub async fn load_attachment(
    path: &Path,
    opts: &AttachOptions,
    model_label: &str,
) -> Result<LoadedAttachment, AttachError> {
    let display = path.display().to_string();
    match classify(path) {
        None => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            Err(AttachError::UnknownExtension { ext })
        }
        Some(AttachmentKind::Image) => {
            if !opts.supports_images {
                return Err(AttachError::ImagesUnsupported {
                    model: model_label.to_string(),
                    path: display,
                });
            }
            let img = sven_image::load_image(path).map_err(|e| AttachError::Image {
                path: display.clone(),
                source: e,
            })?;
            let (w, h) = img.dimensions().unwrap_or((0, 0));
            let mime = img.mime_type.clone();
            let url = img.into_data_url();
            Ok(LoadedAttachment::Image {
                description: format!("Attached image: {display} ({w}x{h}, {mime})"),
                url,
            })
        }
        Some(AttachmentKind::Audio) => {
            let native = opts.supports_audio && !opts.force_transcribe;
            if native {
                let spec = sven_audio::probe(path).map_err(|e| AttachError::Audio {
                    path: display.clone(),
                    source: e,
                })?;
                let url =
                    sven_audio::load_audio_data_url(path).map_err(|e| AttachError::Audio {
                        path: display.clone(),
                        source: e,
                    })?;
                Ok(LoadedAttachment::Audio {
                    description: format!(
                        "Attached audio: {display} ({:.1}s, {} Hz)",
                        spec.duration_secs, spec.sample_rate
                    ),
                    url,
                })
            } else {
                transcribe(path, display, opts, model_label).await
            }
        }
    }
}

/// `path` as text, for a model that cannot take the audio itself.
#[cfg(all(unix, feature = "asr"))]
async fn transcribe(
    path: &Path,
    display: String,
    opts: &AttachOptions,
    _model_label: &str,
) -> Result<LoadedAttachment, AttachError> {
    let client = opts.asr_client.clone().unwrap_or_else(|| {
        sven_model_drivers::dbus::ActionClient::new(opts.asr.bus_address.as_deref())
    });
    let t = asr::transcribe_with(path, &opts.asr, client)
        .await
        .map_err(|e| AttachError::Asr {
            path: display.clone(),
            source: e,
        })?;
    Ok(LoadedAttachment::Transcript {
        text: format!(
            "Transcript of {display} ({:.1}s):\n\n{}",
            t.duration_secs, t.text
        ),
    })
}

/// Without speech-to-text the audio cannot reach the model at all.
#[cfg(not(all(unix, feature = "asr")))]
async fn transcribe(
    _path: &Path,
    display: String,
    _opts: &AttachOptions,
    model_label: &str,
) -> Result<LoadedAttachment, AttachError> {
    Err(AttachError::TranscriptionUnavailable {
        model: model_label.to_string(),
        path: display,
    })
}

// ─── Shared test fixtures ─────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests_support {
    /// A minimal 1×1 red PNG (the same fixture `read_image`'s tests use).
    pub const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// Build a 16-bit mono WAV of `n` silent samples at `rate` Hz.
    pub fn wav_mono_16bit(rate: u32, n: usize) -> Vec<u8> {
        let data = vec![0u8; n * 2];
        let channels: u16 = 1;
        let bits: u16 = 16;
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// Half a second of 16 kHz silence.
    pub fn tiny_wav() -> Vec<u8> {
        wav_mono_16bit(16_000, 8_000)
    }

    /// Serve a fake `Brain1.Manager` that answers `transcribe` with `text`,
    /// and return a client wired to it plus the server connection.
    ///
    /// Peer-to-peer over a `UnixStream` pair: no bus daemon, no well-known
    /// name, no model weights. The server connection is handed back because it
    /// must outlive the call.
    ///
    /// The caller must keep both alive for the duration of the transcription.
    #[cfg(all(unix, feature = "asr"))]
    pub async fn fake_asr(
        text: &str,
    ) -> (sven_model_drivers::dbus::ActionClient, zbus::Connection) {
        struct FakeManager {
            text: String,
        }

        #[zbus::interface(name = "com.swedishembedded.Brain1.Manager")]
        impl FakeManager {
            #[allow(clippy::too_many_arguments)]
            async fn run(
                &self,
                _model: String,
                _action: String,
                _params: String,
                _in_fds: std::collections::HashMap<String, zbus::zvariant::OwnedFd>,
                _in_meta: String,
                _transport: String,
            ) -> zbus::fdo::Result<(
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedFd>,
                String,
            )> {
                // Inline in the result JSON rather than on an fd: brain does
                // this for short replies, and a transcript is short.
                let result = serde_json::json!({ "text": self.text, "num_tokens": 3 }).to_string();
                Ok((result, std::collections::HashMap::new(), "{}".to_string()))
            }

            async fn list_models(&self) -> Vec<String> {
                vec!["brain/nemotronasr".to_string()]
            }
        }

        let (client_sock, server_sock) = tokio::net::UnixStream::pair().expect("socket pair");
        let text = text.to_string();
        // Both `build()` calls drive one half of the same handshake, so they
        // have to run concurrently: awaiting the server first would block on a
        // client that does not exist yet.
        let server_task = tokio::spawn(async move {
            zbus::connection::Builder::unix_stream(server_sock)
                .p2p()
                .server(zbus::Guid::generate())
                .expect("server guid")
                .serve_at("/com/swedishembedded/Brain1", FakeManager { text })
                .expect("serve_at")
                .build()
                .await
                .expect("server connection")
        });
        let client_conn = zbus::connection::Builder::unix_stream(client_sock)
            .p2p()
            .build()
            .await
            .expect("client connection");
        let server_conn = server_task.await.expect("server task");
        (
            sven_model_drivers::dbus::ActionClient::with_connection(client_conn),
            server_conn,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;

    fn opts_text_only() -> AttachOptions {
        AttachOptions::default()
    }

    #[test]
    fn classifies_images_by_extension() {
        assert_eq!(classify(Path::new("/x/a.png")), Some(AttachmentKind::Image));
        assert_eq!(
            classify(Path::new("/x/a.JPEG")),
            Some(AttachmentKind::Image)
        );
    }

    #[test]
    fn classifies_audio_by_extension() {
        assert_eq!(classify(Path::new("/x/a.wav")), Some(AttachmentKind::Audio));
        assert_eq!(classify(Path::new("/x/a.MP3")), Some(AttachmentKind::Audio));
    }

    #[test]
    fn rejects_unknown_extensions() {
        assert_eq!(classify(Path::new("/x/a.rs")), None);
        assert_eq!(classify(Path::new("/x/noext")), None);
    }

    #[tokio::test]
    async fn unknown_extension_errors() {
        let err = load_attachment(Path::new("x.rs"), &opts_text_only(), "m")
            .await
            .unwrap_err();
        assert!(matches!(err, AttachError::UnknownExtension { .. }));
        assert!(err.to_string().contains("png"), "{err}");
    }

    #[tokio::test]
    async fn image_refused_when_model_has_no_vision() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        std::fs::write(&png, MINIMAL_PNG).unwrap();
        let err = load_attachment(&png, &opts_text_only(), "text-only-model")
            .await
            .unwrap_err();
        assert!(matches!(err, AttachError::ImagesUnsupported { .. }));
        assert!(err.to_string().contains("text-only-model"), "{err}");
    }

    #[tokio::test]
    async fn image_loads_to_image_part() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        std::fs::write(&png, MINIMAL_PNG).unwrap();
        let opts = AttachOptions {
            supports_images: true,
            ..AttachOptions::default()
        };
        let loaded = load_attachment(&png, &opts, "vision-model").await.unwrap();
        assert!(
            loaded.text().contains("Attached image"),
            "{}",
            loaded.text()
        );
        assert!(loaded.text().contains("1x1"), "{}", loaded.text());

        let parts = loaded.into_content_parts();
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[1], ContentPart::Image { .. }));
    }

    #[tokio::test]
    async fn native_audio_loads_to_audio_part() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let opts = AttachOptions {
            supports_audio: true,
            ..AttachOptions::default()
        };
        let loaded = load_attachment(&wav, &opts, "omni").await.unwrap();
        assert!(
            loaded.text().contains("Attached audio"),
            "{}",
            loaded.text()
        );
        assert!(loaded.text().contains("16000 Hz"), "{}", loaded.text());
        assert!(loaded.text().contains("0.5s"), "{}", loaded.text());

        let parts = loaded.into_tool_parts();
        assert_eq!(parts.len(), 2);
        match &parts[1] {
            ToolOutputPart::Audio(url) => assert!(url.starts_with("data:audio/wav;base64,")),
            other => panic!("expected an audio part, got {other:?}"),
        }
    }

    #[cfg(all(unix, feature = "asr"))]
    #[tokio::test]
    async fn audio_falls_back_to_transcript_without_native_support() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let (client, _server) = fake_asr("draw a box around the cat").await;
        let opts = AttachOptions {
            asr_client: Some(client),
            ..AttachOptions::default()
        };
        let loaded = load_attachment(&wav, &opts, "text-model").await.unwrap();
        assert!(matches!(loaded, LoadedAttachment::Transcript { .. }));
        assert!(loaded.text().contains("Transcript of"), "{}", loaded.text());
        assert!(
            loaded.text().contains("draw a box around the cat"),
            "{}",
            loaded.text()
        );
        // Text-only output flows through any provider.
        let parts = loaded.into_content_parts();
        assert_eq!(parts.len(), 1);
        assert!(matches!(parts[0], ContentPart::Text { .. }));
    }

    #[cfg(all(unix, feature = "asr"))]
    #[tokio::test]
    async fn force_transcribe_overrides_native_audio_support() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let (client, _server) = fake_asr("forced").await;
        let opts = AttachOptions {
            supports_audio: true,
            force_transcribe: true,
            asr_client: Some(client),
            ..AttachOptions::default()
        };
        let loaded = load_attachment(&wav, &opts, "omni").await.unwrap();
        assert!(matches!(loaded, LoadedAttachment::Transcript { .. }));
    }

    #[cfg(all(unix, feature = "asr"))]
    #[tokio::test]
    /// An unreachable brain must say which model it was trying to use and
    /// carry the transport reason underneath. "Transcription failed" alone
    /// cannot distinguish a stopped server from a misspelled model id, and
    /// both are common.
    async fn asr_failure_names_the_model_and_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let opts = AttachOptions {
            asr: AsrConfig {
                model: "brain/nemotronasr".into(),
                // Nothing is listening here, so the call cannot connect.
                bus_address: Some(format!(
                    "unix:path={}",
                    dir.path().join("absent.sock").display()
                )),
                timeout_secs: 5,
            },
            ..AttachOptions::default()
        };
        let err = load_attachment(&wav, &opts, "text-model")
            .await
            .unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("brain/nemotronasr"), "{chain}");
    }

    /// Without speech-to-text, audio for a model that cannot hear is refused
    /// with the model and the missing capability named, rather than dropped.
    #[cfg(not(all(unix, feature = "asr")))]
    #[tokio::test]
    async fn audio_is_refused_where_it_would_need_transcription_and_none_is_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, tiny_wav()).unwrap();
        let err = load_attachment(&wav, &AttachOptions::default(), "text-model")
            .await
            .unwrap_err();
        assert!(
            matches!(err, AttachError::TranscriptionUnavailable { .. }),
            "got {err:?}"
        );
        let message = err.to_string();
        assert!(message.contains("text-model"), "{message}");
        assert!(message.contains("`asr` feature"), "{message}");
    }

    #[tokio::test]
    async fn corrupt_wav_reports_an_audio_error() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, b"not a wav").unwrap();
        let opts = AttachOptions {
            supports_audio: true,
            ..AttachOptions::default()
        };
        let err = load_attachment(&wav, &opts, "omni").await.unwrap_err();
        assert!(matches!(err, AttachError::Audio { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn missing_image_file_reports_an_image_error() {
        let opts = AttachOptions {
            supports_images: true,
            ..AttachOptions::default()
        };
        let err = load_attachment(Path::new("nope_xyz.png"), &opts, "vision")
            .await
            .unwrap_err();
        assert!(matches!(err, AttachError::Image { .. }), "got {err:?}");
    }
}
