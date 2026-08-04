// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Speech-to-text fallback used when the active model cannot accept audio.
//!
//! Transcription shells out to the `brain` CLI:
//!
//! ```text
//! brain do <asr_model> transcribe --json --in audio=<file>
//! ```
//!
//! ## Why the temp file is raw PCM, not the original WAV
//!
//! `brain do`'s blob loader reads non-image `--in` files **completely raw**,
//! with no format sniffing.  Handing it a `.wav` would feed the 44-byte RIFF
//! header to the model as if it were audio samples.  The input is therefore
//! written as headerless mono `f32` little-endian PCM at 16 kHz — exactly the
//! sample layout the loader assumes — under a `.pcm` suffix.

use std::path::Path;
use std::process::Stdio;

use sven_config::AsrConfig;
use thiserror::Error;
use tokio::process::Command;
use tracing::debug;

/// Sample rate the ASR model expects.
pub const ASR_SAMPLE_RATE: u32 = 16_000;

/// How many bytes of stderr to quote back in an error message.
const STDERR_TAIL_BYTES: usize = 512;

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("could not decode audio for transcription: {0}")]
    Decode(#[from] sven_audio::AudioError),

    #[error("could not stage audio for transcription: {0}")]
    Staging(#[source] std::io::Error),

    #[error("could not run transcription command '{command}': {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("transcription command '{command}' timed out after {timeout_secs}s")]
    Timeout { command: String, timeout_secs: u64 },

    #[error("transcription command '{command}' exited with {status}: {stderr}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },

    #[error("transcription command '{command}' produced no parsable JSON on stdout: {detail}")]
    BadOutput { command: String, detail: String },
}

/// A completed transcription.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// The recognised text.
    pub text: String,
    /// Duration of the source audio in seconds.
    pub duration_secs: f32,
}

/// Transcribe `path` by invoking the configured ASR command.
pub async fn transcribe(path: &Path, cfg: &AsrConfig) -> Result<Transcript, AsrError> {
    let pcm = sven_audio::load_pcm_at(path, ASR_SAMPLE_RATE)?;
    let duration_secs = pcm.duration_secs();
    let bytes = sven_audio::to_f32_le_bytes(&pcm.samples);

    // Headerless raw samples, `.pcm` suffix — see the module docs.
    let tmp = tempfile::Builder::new()
        .prefix("sven-asr-")
        .suffix(".pcm")
        .tempfile()
        .map_err(AsrError::Staging)?;
    std::fs::write(tmp.path(), &bytes).map_err(AsrError::Staging)?;

    let text = run_asr_command(tmp.path(), cfg).await?;
    Ok(Transcript {
        text,
        duration_secs,
    })
}

/// Spawn the ASR subprocess and extract `.text` from its JSON output.
async fn run_asr_command(pcm_path: &Path, cfg: &AsrConfig) -> Result<String, AsrError> {
    let args = [
        "do".to_string(),
        cfg.model.clone(),
        "transcribe".to_string(),
        "--json".to_string(),
        "--in".to_string(),
        format!("audio={}", pcm_path.display()),
    ];
    debug!(command = %cfg.command, ?args, "running ASR subprocess");

    let child = Command::new(&cfg.command)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AsrError::Spawn {
            command: cfg.command.clone(),
            source: e,
        })?;

    let timeout = std::time::Duration::from_secs(cfg.timeout_secs.max(1));
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => {
            return Err(AsrError::Timeout {
                command: cfg.command.clone(),
                timeout_secs: cfg.timeout_secs,
            })
        }
        Ok(Err(e)) => {
            return Err(AsrError::Spawn {
                command: cfg.command.clone(),
                source: e,
            })
        }
        Ok(Ok(o)) => o,
    };

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(AsrError::Failed {
            command: cfg.command.clone(),
            status: output.status.to_string(),
            stderr: tail(&stderr, STDERR_TAIL_BYTES),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    parse_transcript_json(&stdout).map_err(|detail| AsrError::BadOutput {
        command: cfg.command.clone(),
        detail: format!("{detail}; stderr: {}", tail(&stderr, STDERR_TAIL_BYTES)),
    })
}

/// Extract the `text` field from `brain do --json` output.
///
/// The command prints exactly one line of JSON shaped
/// `{"text": "...", "tokens": [...], "num_tokens": N}`, but tolerate leading
/// progress lines by scanning backwards for the last parsable JSON object.
pub(crate) fn parse_transcript_json(stdout: &str) -> Result<String, String> {
    let mut saw_json = false;
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_json = true;
        if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
            return Ok(text.to_string());
        }
    }
    if saw_json {
        Err("JSON output has no string `text` field".to_string())
    } else {
        Err(format!(
            "no JSON line found in stdout ({} bytes)",
            stdout.len()
        ))
    }
}

/// Return at most the last `max` bytes of `s`, on a char boundary.
fn tail(s: &str, max: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_from_single_json_line() {
        let out = r#"{"text": "hello world", "tokens": [1,2], "num_tokens": 2}"#;
        assert_eq!(parse_transcript_json(out).unwrap(), "hello world");
    }

    #[test]
    fn parses_text_ignoring_leading_progress_lines() {
        let out = "loading model...\nready\n{\"text\": \"ok\", \"num_tokens\": 1}\n";
        assert_eq!(parse_transcript_json(out).unwrap(), "ok");
    }

    #[test]
    fn missing_text_field_is_an_error() {
        let err = parse_transcript_json(r#"{"tokens": []}"#).unwrap_err();
        assert!(err.contains("text"), "got {err}");
    }

    #[test]
    fn no_json_at_all_is_an_error() {
        let err = parse_transcript_json("segfault\n").unwrap_err();
        assert!(err.contains("no JSON line"), "got {err}");
    }

    #[test]
    fn empty_output_is_an_error() {
        assert!(parse_transcript_json("").is_err());
    }

    #[test]
    fn tail_keeps_the_end_of_long_output() {
        let s = "a".repeat(100);
        let t = tail(&s, 10);
        assert!(t.starts_with('…'));
        assert_eq!(t.chars().filter(|c| *c == 'a').count(), 10);
    }

    #[test]
    fn tail_returns_short_input_unchanged() {
        assert_eq!(tail("boom\n", 512), "boom");
    }

    #[tokio::test]
    async fn missing_command_reports_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(
            &wav,
            crate::builtin::file::attachment::tests_support::tiny_wav(),
        )
        .unwrap();
        let cfg = AsrConfig {
            command: "/nonexistent/sven-asr-binary".into(),
            model: "m".into(),
            timeout_secs: 5,
        };
        let err = transcribe(&wav, &cfg).await.unwrap_err();
        assert!(matches!(err, AsrError::Spawn { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn non_wav_input_fails_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("a.mp3");
        std::fs::write(&mp3, b"ID3").unwrap();
        let cfg = AsrConfig::default();
        let err = transcribe(&mp3, &cfg).await.unwrap_err();
        assert!(matches!(err, AsrError::Decode(_)), "got {err:?}");
    }
}
