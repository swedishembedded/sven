// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in streaming
// speech recognition then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Speech to text from an OpenAI-compatible server: `POST {base}/audio/transcriptions`.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use reqwest::multipart::{Form, Part};
use serde_json::Value;

/// How long one transcription request may take, connect to last byte. A
/// phrase is at most 8 s of audio, so a slower answer is a stuck server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Transcribes one Ogg/Opus recording.
///
/// `base_url` is the server's OpenAI-style base (it ends in `/v1`); the audio
/// goes as the multipart `file`, with the `model` and, when given, the
/// `language` (ISO-639-1). Returns the `text` of the answer.
///
/// # Errors
/// The server cannot be reached or does not answer in time, answers a status
/// that is not a success (the message names it and carries the server's own
/// `error.message` when there is one), or answers without a `text`.
pub async fn transcribe(
    base_url: &str,
    api_key: &str,
    model: &str,
    language: Option<&str>,
    ogg: Vec<u8>,
) -> anyhow::Result<String> {
    let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
    let file = Part::bytes(ogg)
        .file_name("speech.ogg")
        .mime_str("audio/ogg")
        .context("audio part")?;
    let mut form = Form::new()
        .part("file", file)
        .text("model", model.to_owned());
    if let Some(language) = language {
        form = form.text("language", language.to_owned());
    }
    let response = crate::build_http_client()
        .post(&url)
        .bearer_auth(api_key)
        .timeout(REQUEST_TIMEOUT)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("transcription request to {url}"))?;
    let status = response.status();
    let body = response.text().await.context("transcription answer")?;
    let json: Option<Value> = serde_json::from_str(&body).ok();
    if !status.is_success() {
        let message = json
            .as_ref()
            .and_then(|j| j["error"]["message"].as_str())
            .map_or_else(|| body.chars().take(200).collect(), str::to_owned);
        bail!("transcription failed: HTTP {}: {message}", status.as_u16());
    }
    json.as_ref()
        .and_then(|j| j["text"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("transcription answer has no text"))
}
