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
    let mut fields = vec![("model", model)];
    if let Some(language) = language {
        fields.push(("language", language));
    }
    let (content_type, body) = form_data(&fields, &ogg);
    let response = crate::build_http_client()
        .post(&url)
        .bearer_auth(api_key)
        .timeout(REQUEST_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
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

/// A `multipart/form-data` body (RFC 7578) of `fields` and the recording as
/// the `file` part (`speech.ogg`, `audio/ogg`), and its content type. Built
/// here rather than by reqwest's multipart support, which would link a
/// table guessing a type from every file extension for this one known part.
fn form_data(fields: &[(&str, &str)], ogg: &[u8]) -> (String, Vec<u8>) {
    let boundary = boundary_absent_from(ogg, fields);
    let mut body = Vec::with_capacity(ogg.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.ogg\"\r\nContent-Type: audio/ogg\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(ogg);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A boundary that occurs in none of the parts: random, and drawn again in
/// the unlikely case it does.
fn boundary_absent_from(ogg: &[u8], fields: &[(&str, &str)]) -> String {
    use std::hash::{BuildHasher, Hasher};
    loop {
        let random = || {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        let boundary = format!("sven-{:016x}{:016x}", random(), random());
        let needle = boundary.as_bytes();
        let occurs = |haystack: &[u8]| haystack.windows(needle.len()).any(|w| w == needle);
        if !occurs(ogg) && !fields.iter().any(|(_, v)| occurs(v.as_bytes())) {
            return boundary;
        }
    }
}
