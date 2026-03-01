// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! AWS Bedrock driver — native Converse API with SigV4 authentication.
//!
//! Uses the non-streaming `POST /model/{modelId}/converse` endpoint and wraps
//! the response into the standard `ResponseStream`.  The full SigV4 signing
//! algorithm is implemented locally using `sha2` and `hex` (already workspace
//! dependencies) to avoid pulling in the AWS SDK.
//!
//! # Credentials
//! Reads from env vars:
//! - `AWS_ACCESS_KEY_ID`
//! - `AWS_SECRET_ACCESS_KEY`
//! - `AWS_SESSION_TOKEN` (optional, for temporary credentials)
//! - `AWS_DEFAULT_REGION` or `AWS_REGION` (fallback: `us-east-1`)
//!
//! # Model IDs
//! Use Bedrock cross-region inference profile IDs or regional model IDs, e.g.:
//! - `us.anthropic.claude-3-5-sonnet-20241022-v2:0`
//! - `amazon.nova-pro-v1:0`

use anyhow::{bail, Context};
use async_trait::async_trait;
use chrono::Utc;
use futures::stream;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::{
    catalog::{static_catalog, ModelCatalogEntry},
    provider::ResponseStream,
    CompletionRequest, MessageContent, ResponseEvent, Role,
};

pub struct BedrockProvider {
    model: String,
    region: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl BedrockProvider {
    pub fn new(
        model: String,
        region: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        let region = region
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
            .or_else(|| std::env::var("AWS_REGION").ok())
            .unwrap_or_else(|| "us-east-1".into());
        Self {
            model,
            region,
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for BedrockProvider {
    fn name(&self) -> &str {
        "aws"
    }
    fn model_name(&self) -> &str {
        &self.model
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let mut entries: Vec<ModelCatalogEntry> = static_catalog()
            .into_iter()
            .filter(|e| e.provider == "aws")
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let access_key = std::env::var("AWS_ACCESS_KEY_ID").context("AWS_ACCESS_KEY_ID not set")?;
        let secret_key =
            std::env::var("AWS_SECRET_ACCESS_KEY").context("AWS_SECRET_ACCESS_KEY not set")?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();

        // Separate system messages
        let mut system_parts: Vec<Value> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for m in &req.messages {
            if m.role == Role::System {
                if let Some(t) = m.as_text() {
                    // Bedrock Converse has a single system array; append dynamic
                    // context to the first system text block.
                    if let Some(suffix) = &req.system_dynamic_suffix {
                        if !suffix.trim().is_empty() {
                            system_parts.push(json!({ "text": format!("{t}\n\n{suffix}") }));
                            continue;
                        }
                    }
                    system_parts.push(json!({ "text": t }));
                }
                continue;
            }
            let role = match m.role {
                Role::User | Role::Tool => "user",
                Role::Assistant => "assistant",
                Role::System => unreachable!(),
            };
            let content = match &m.content {
                MessageContent::Text(t) => vec![json!({ "text": t })],
                MessageContent::ContentParts(parts) => parts
                    .iter()
                    .map(|p| match p {
                        crate::ContentPart::Text { text } => json!({ "text": text }),
                        crate::ContentPart::Image { image_url, .. } => {
                            if let Ok((mime, b64)) = crate::types::parse_data_url_parts(image_url) {
                                let format = normalize_bedrock_image_format(&mime);
                                json!({
                                    "image": {
                                        "format": format,
                                        "source": { "bytes": b64 },
                                    }
                                })
                            } else {
                                json!({ "text": format!("[image: {}]", image_url) })
                            }
                        }
                    })
                    .collect(),
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } => {
                    let input: Value =
                        serde_json::from_str(&function.arguments).unwrap_or(json!({}));
                    vec![json!({
                        "toolUse": {
                            "toolUseId": tool_call_id,
                            "name": function.name,
                            "input": input,
                        }
                    })]
                }
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    let bedrock_content: Vec<Value> = match content {
                        crate::ToolResultContent::Text(t) => vec![json!({ "text": t })],
                        crate::ToolResultContent::Parts(parts) => parts
                            .iter()
                            .map(|p| match p {
                                crate::ToolContentPart::Text { text } => json!({ "text": text }),
                                crate::ToolContentPart::Image { image_url } => {
                                    if let Ok((mime, b64)) =
                                        crate::types::parse_data_url_parts(image_url)
                                    {
                                        let format = normalize_bedrock_image_format(&mime);
                                        json!({
                                            "image": {
                                                "format": format,
                                                "source": { "bytes": b64 },
                                            }
                                        })
                                    } else {
                                        json!({ "text": format!("[image: {}]", image_url) })
                                    }
                                }
                            })
                            .collect(),
                    };
                    vec![json!({
                        "toolResult": {
                            "toolUseId": tool_call_id,
                            "content": bedrock_content,
                        }
                    })]
                }
            };
            messages.push(json!({ "role": role, "content": content }));
        }

        // Tool config
        let tool_config = if req.tools.is_empty() {
            None
        } else {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "toolSpec": {
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": { "json": t.parameters },
                        }
                    })
                })
                .collect();
            Some(json!({ "tools": tools }))
        };

        let mut body = json!({
            "messages": messages,
            "inferenceConfig": {
                "maxTokens": self.max_tokens,
                "temperature": self.temperature,
            }
        });
        if !system_parts.is_empty() {
            body["system"] = json!(system_parts);
        }
        if let Some(tc) = tool_config {
            body["toolConfig"] = tc;
        }

        let body_bytes = serde_json::to_vec(&body)?;
        let url = format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
            self.region,
            urlencoded(&self.model),
        );

        debug!(model = %self.model, region = %self.region, "sending AWS Bedrock request");

        let now = Utc::now();
        let date_time = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = &date_time[..8];
        let host = format!("bedrock-runtime.{}.amazonaws.com", self.region);
        let content_type = "application/json";
        let service = "bedrock";

        let headers_to_sign: Vec<(&str, &str)> = {
            let mut h = vec![
                ("content-type", content_type),
                ("host", host.as_str()),
                ("x-amz-date", date_time.as_str()),
            ];
            if let Some(tok) = &session_token {
                h.push(("x-amz-security-token", tok.as_str()));
            }
            h.sort_by_key(|&(k, _)| k);
            h
        };

        let canonical_headers: String = headers_to_sign
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k.to_lowercase(), v.trim()))
            .collect();
        let signed_headers: String = headers_to_sign
            .iter()
            .map(|(k, _)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(";");
        let body_hash = hex_sha256(&body_bytes);

        let path = format!("/model/{}/converse", urlencoded(&self.model));
        let canonical_request = format!(
            "POST\n{}\n\n{}\n{}\n{}",
            path, canonical_headers, signed_headers, body_hash
        );

        let credential_scope = format!("{}/{}/{}/aws4_request", date, self.region, service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            date_time,
            credential_scope,
            hex_sha256(canonical_request.as_bytes())
        );

        let signing_key = derive_signing_key(secret_key.as_bytes(), date, &self.region, service);
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
            access_key, credential_scope, signed_headers, signature
        );

        let mut req_builder = self
            .client
            .post(&url)
            .header("content-type", content_type)
            .header("host", &host)
            .header("x-amz-date", &date_time)
            .header("Authorization", &authorization)
            .body(body_bytes);

        if let Some(tok) = &session_token {
            req_builder = req_builder.header("x-amz-security-token", tok);
        }

        let resp = req_builder
            .send()
            .await
            .context("AWS Bedrock request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("AWS Bedrock error {status}: {text}");
        }

        let response_body: Value = resp
            .json()
            .await
            .context("AWS Bedrock response parse failed")?;

        // Convert the synchronous Converse response into a stream of events.
        let mut events: Vec<anyhow::Result<ResponseEvent>> = Vec::new();

        if let Some(output) = response_body.get("output") {
            if let Some(message) = output.get("message") {
                if let Some(content_arr) = message["content"].as_array() {
                    for part in content_arr {
                        if let Some(text) = part["text"].as_str() {
                            if !text.is_empty() {
                                events.push(Ok(ResponseEvent::TextDelta(text.to_string())));
                            }
                        }
                        if let Some(tu) = part.get("toolUse") {
                            let id = tu["toolUseId"].as_str().unwrap_or("").to_string();
                            let name = tu["name"].as_str().unwrap_or("").to_string();
                            let args = serde_json::to_string(&tu["input"]).unwrap_or_default();
                            events.push(Ok(ResponseEvent::ToolCall {
                                index: 0,
                                id,
                                name,
                                arguments: args,
                            }));
                        }
                        // Claude Extended Thinking via AWS Bedrock Converse API:
                        // thinking content arrives as a `reasoningContent` block
                        // containing a nested `reasoningText.text` field.
                        // The accompanying `reasoningText.signature` is an
                        // encrypted integrity blob — not human-readable; discard it.
                        if let Some(rc) = part.get("reasoningContent") {
                            if let Some(rt) = rc.get("reasoningText") {
                                if let Some(text) = rt["text"].as_str() {
                                    if !text.is_empty() {
                                        events.push(Ok(ResponseEvent::ThinkingDelta(
                                            text.to_string(),
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(usage) = response_body.get("usage") {
            events.push(Ok(ResponseEvent::Usage {
                input_tokens: usage["inputTokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: usage["outputTokens"].as_u64().unwrap_or(0) as u32,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }));
        }

        // AWS Bedrock Converse API reports stopReason="max_tokens" when the
        // output was cut off by the token limit.
        if response_body["stopReason"].as_str() == Some("max_tokens") {
            events.push(Ok(ResponseEvent::MaxTokens));
        }

        events.push(Ok(ResponseEvent::Done));

        Ok(Box::pin(stream::iter(events)))
    }
}

// ── SigV4 helpers ─────────────────────────────────────────────────────────────

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

fn hex_sha256(data: &[u8]) -> String {
    hex::encode(sha256(data))
}

/// HMAC-SHA256 computed without the `hmac` crate using the raw SHA256 primitive.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let norm_key = if key.len() > BLOCK {
        sha256(key)
    } else {
        key.to_vec()
    };
    let mut padded = [0u8; BLOCK];
    padded[..norm_key.len()].copy_from_slice(&norm_key);
    let ipad: Vec<u8> = padded.iter().map(|&b| b ^ 0x36).collect();
    let opad: Vec<u8> = padded.iter().map(|&b| b ^ 0x5c).collect();
    let inner = {
        let mut h = Sha256::new();
        h.update(&ipad);
        h.update(data);
        h.finalize().to_vec()
    };
    let mut h = Sha256::new();
    h.update(&opad);
    h.update(&inner);
    h.finalize().to_vec()
}

fn derive_signing_key(secret: &[u8], date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = [b"AWS4", secret].concat();
    let k_date = hmac_sha256(&k_secret, date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// URL-encode a string (percent-encode non-unreserved characters, except `/`
/// which appears in model IDs like `anthropic.claude:0` → `:` encoded).
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Map an image MIME type to the format string expected by Bedrock's Converse API.
///
/// Bedrock accepts: `jpeg`, `png`, `gif`, `webp`.
/// Normalizes non-standard aliases (`jpg` → `jpeg`).
fn normalize_bedrock_image_format(mime: &str) -> String {
    let raw = mime.strip_prefix("image/").unwrap_or("jpeg");
    match raw {
        "jpg" => "jpeg".to_string(),
        // gif, webp, png, jpeg — all valid Bedrock formats, pass through
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;

    #[test]
    fn hmac_sha256_known_vector() {
        // Test vector from HMAC-SHA256 RFC 4231 test case 1
        let key = b"key";
        let data = b"The quick brown fox jumps over the lazy dog";
        let result = hex::encode(hmac_sha256(key, data));
        // Known good value
        assert_eq!(
            result,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn derive_signing_key_is_deterministic() {
        let k1 = derive_signing_key(b"secret", "20240101", "us-east-1", "bedrock");
        let k2 = derive_signing_key(b"secret", "20240101", "us-east-1", "bedrock");
        assert_eq!(k1, k2);
    }

    #[test]
    fn urlencoded_safe_chars_unchanged() {
        assert_eq!(
            urlencoded("us.anthropic.claude-3-5/v2"),
            "us.anthropic.claude-3-5/v2"
        );
    }

    #[test]
    fn urlencoded_colon_encoded() {
        assert_eq!(urlencoded("model:0"), "model%3A0");
    }

    // ── normalize_bedrock_image_format ─────────────────────────────────────────

    #[test]
    fn bedrock_format_jpeg_passthrough() {
        assert_eq!(normalize_bedrock_image_format("image/jpeg"), "jpeg");
    }

    #[test]
    fn bedrock_format_png_passthrough() {
        assert_eq!(normalize_bedrock_image_format("image/png"), "png");
    }

    #[test]
    fn bedrock_format_jpg_normalized_to_jpeg() {
        assert_eq!(normalize_bedrock_image_format("image/jpg"), "jpeg");
    }

    #[test]
    fn bedrock_format_gif_passthrough() {
        assert_eq!(normalize_bedrock_image_format("image/gif"), "gif");
    }

    #[test]
    fn bedrock_format_webp_passthrough() {
        assert_eq!(normalize_bedrock_image_format("image/webp"), "webp");
    }

    #[test]
    fn provider_defaults() {
        let p = BedrockProvider::new(
            "amazon.nova-pro-v1:0".into(),
            Some("eu-west-1".into()),
            None,
            None,
        );
        assert_eq!(p.name(), "aws");
        assert_eq!(p.region, "eu-west-1");
        assert_eq!(p.max_tokens, 4096);
    }

    // ── reasoningContent (Claude Extended Thinking via Bedrock) ───────────────

    /// Helper: parse a Bedrock Converse response body into a flat event list.
    fn parse_bedrock_events(body: serde_json::Value) -> Vec<crate::ResponseEvent> {
        let mut events: Vec<anyhow::Result<crate::ResponseEvent>> = Vec::new();

        if let Some(output) = body.get("output") {
            if let Some(message) = output.get("message") {
                if let Some(content_arr) = message["content"].as_array() {
                    for part in content_arr {
                        if let Some(text) = part["text"].as_str() {
                            if !text.is_empty() {
                                events.push(Ok(crate::ResponseEvent::TextDelta(text.to_string())));
                            }
                        }
                        if let Some(rc) = part.get("reasoningContent") {
                            if let Some(rt) = rc.get("reasoningText") {
                                if let Some(text) = rt["text"].as_str() {
                                    if !text.is_empty() {
                                        events.push(Ok(crate::ResponseEvent::ThinkingDelta(
                                            text.to_string(),
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        events.into_iter().filter_map(|r| r.ok()).collect()
    }

    #[test]
    fn bedrock_reasoning_content_produces_thinking_delta() {
        // Claude Extended Thinking on Bedrock: reasoning arrives in a
        // `reasoningContent` block with a nested `reasoningText.text`.
        let body = serde_json::json!({
            "output": {
                "message": {
                    "content": [{
                        "reasoningContent": {
                            "reasoningText": {
                                "text": "Let me think step by step.",
                                "signature": "EqRkLm..."
                            }
                        }
                    }]
                }
            }
        });
        let events = parse_bedrock_events(body);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], crate::ResponseEvent::ThinkingDelta(t) if t == "Let me think step by step."),
            "expected ThinkingDelta, got {:?}",
            events[0]
        );
    }

    #[test]
    fn bedrock_reasoning_content_and_text_both_emitted_in_order() {
        // A response with reasoning first, then the answer.
        let body = serde_json::json!({
            "output": {
                "message": {
                    "content": [
                        {
                            "reasoningContent": {
                                "reasoningText": {
                                    "text": "I need to reason here.",
                                    "signature": "sig"
                                }
                            }
                        },
                        { "text": "The final answer is 42." }
                    ]
                }
            }
        });
        let events = parse_bedrock_events(body);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], crate::ResponseEvent::ThinkingDelta(_)));
        assert!(matches!(&events[1], crate::ResponseEvent::TextDelta(t) if t.contains("42")));
    }

    #[test]
    fn bedrock_reasoning_content_without_reasoning_text_ignored() {
        // A reasoningContent block without reasoningText should not crash.
        let body = serde_json::json!({
            "output": {
                "message": {
                    "content": [{
                        "reasoningContent": {}
                    }]
                }
            }
        });
        let events = parse_bedrock_events(body);
        assert!(
            events.is_empty(),
            "no events expected for empty reasoningContent"
        );
    }

    #[test]
    fn bedrock_signature_not_emitted() {
        // The `signature` field in `reasoningText` is an encrypted blob and
        // must never be surfaced as a thinking or text event.
        let body = serde_json::json!({
            "output": {
                "message": {
                    "content": [{
                        "reasoningContent": {
                            "reasoningText": {
                                "text": "",
                                "signature": "EqRkLmSomeEncryptedBlob"
                            }
                        }
                    }]
                }
            }
        });
        let events = parse_bedrock_events(body);
        // Empty text → no events; signature is silently discarded.
        assert!(events.is_empty(), "signature blob must not produce events");
    }
}
