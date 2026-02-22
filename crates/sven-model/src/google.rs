// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Google Gemini driver — native Generative Language API.
//!
//! Uses the `generateContent` / `streamGenerateContent` endpoints.
//! Supports text, tool calls, and thinking deltas via `thought` parts.
//!
//! # Auth
//! API key via `x-goog-api-key` header (or `?key=...` query param).
//!
//! # Endpoint pattern
//! `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse`

use std::collections::HashMap;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

use crate::{
    catalog::{static_catalog, ModelCatalogEntry},
    provider::ResponseStream,
    CompletionRequest, MessageContent, ResponseEvent, Role,
};

pub struct GoogleProvider {
    model: String,
    api_key: Option<String>,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl GoogleProvider {
    pub fn new(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://generativelanguage.googleapis.com".into()),
            max_tokens: max_tokens.unwrap_or(8192),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for GoogleProvider {
    fn name(&self) -> &str { "google" }
    fn model_name(&self) -> &str { &self.model }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let mut entries: Vec<ModelCatalogEntry> = static_catalog()
            .into_iter()
            .filter(|e| e.provider == "google")
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let key = self.api_key.as_deref().context("GEMINI_API_KEY not set")?;

        // Separate system instruction from conversation.
        // Also build a mapping from tool_call_id → function_name so that
        // functionResponse parts can use the correct function name (Gemini
        // matches responses to calls by name, not by ID).
        let mut system_parts: Vec<Value> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();
        let mut tc_name_map: HashMap<String, String> = HashMap::new();

        for m in &req.messages {
            if let MessageContent::ToolCall { tool_call_id, function } = &m.content {
                tc_name_map.insert(tool_call_id.clone(), function.name.clone());
            }
        }

        for m in &req.messages {
            match m.role {
                Role::System => {
                    if let Some(t) = m.as_text() {
                        // Append dynamic context directly to the system text;
                        // Gemini does not have a separate uncached-block concept.
                        if let Some(suffix) = &req.system_dynamic_suffix {
                            if !suffix.trim().is_empty() {
                                system_parts.push(json!({ "text": format!("{t}\n\n{suffix}") }));
                                continue;
                            }
                        }
                        system_parts.push(json!({ "text": t }));
                    }
                }
                Role::User | Role::Tool => {
                    let parts = message_to_gemini_parts(m, &tc_name_map);
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
                Role::Assistant => {
                    let parts = message_to_gemini_parts(m, &tc_name_map);
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
        }

        // Tool declarations
        let tools_section: Option<Value> = if req.tools.is_empty() {
            None
        } else {
            let function_declarations: Vec<Value> = req.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })).collect();
            Some(json!([{ "functionDeclarations": function_declarations }]))
        };

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": self.max_tokens,
                "temperature": self.temperature,
            }
        });
        if !system_parts.is_empty() {
            body["systemInstruction"] = json!({ "parts": system_parts });
        }
        if let Some(tools) = tools_section {
            body["tools"] = tools;
        }

        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url.trim_end_matches('/'),
            self.model,
            key
        );

        debug!(model = %self.model, "sending Google Gemini request");

        let resp = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Google Gemini request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Google Gemini error {status}: {text}");
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = byte_stream.flat_map(|chunk| {
            let lines = match chunk {
                Ok(b) => String::from_utf8_lossy(&b).to_string(),
                Err(e) => return futures::stream::iter(vec![Err(anyhow::anyhow!(e))]),
            };
            let events: Vec<anyhow::Result<ResponseEvent>> = lines
                .lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("data: ")?.trim();
                    if line == "[DONE]" {
                        return Some(Ok(ResponseEvent::Done));
                    }
                    let v: Value = serde_json::from_str(line).ok()?;
                    Some(parse_gemini_chunk(&v))
                })
                .collect();
            futures::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
}

/// Convert a sven message into Gemini API `parts` array.
///
/// `tc_name_map` maps `tool_call_id → function_name` so that `functionResponse`
/// parts can carry the correct function name (Gemini matches responses to calls
/// by function name, not by the opaque call ID).
fn message_to_gemini_parts(
    m: &crate::Message,
    tc_name_map: &HashMap<String, String>,
) -> Vec<Value> {
    match &m.content {
        MessageContent::Text(t) => vec![json!({ "text": t })],
        MessageContent::ContentParts(parts) => {
            if parts.is_empty() {
                return vec![json!({ "text": "" })];
            }
            parts.iter().map(|p| match p {
                crate::ContentPart::Text { text } => json!({ "text": text }),
                crate::ContentPart::Image { image_url, .. } => {
                    if let Ok((mime, data)) = crate::types::parse_data_url_parts(image_url) {
                        json!({
                            "inline_data": {
                                "mime_type": mime,
                                "data": data,
                            }
                        })
                    } else {
                        // Remote URL
                        json!({ "file_data": { "file_uri": image_url } })
                    }
                }
            }).collect()
        }
        MessageContent::ToolCall { tool_call_id: _, function } => {
            let input: Value = serde_json::from_str(&function.arguments).unwrap_or(json!({}));
            vec![json!({
                "functionCall": {
                    "name": function.name,
                    "args": input,
                }
            })]
        }
        MessageContent::ToolResult { tool_call_id, content } => {
            // Resolve the function name — Gemini matches functionResponse to
            // functionCall by the "name" field, not by an opaque ID.
            let fn_name = tc_name_map
                .get(tool_call_id)
                .map(|s| s.as_str())
                .unwrap_or(tool_call_id);  // fallback to ID if name unknown

            match content {
                crate::ToolResultContent::Text(t) => {
                    vec![json!({
                        "functionResponse": {
                            "name": fn_name,
                            "response": { "output": t },
                        }
                    })]
                }
                crate::ToolResultContent::Parts(parts) => {
                    // Gemini functionResponse carries text in "output".
                    // Images are emitted as separate inline_data parts alongside
                    // the functionResponse part.
                    let output_text: String = parts.iter()
                        .filter_map(|p| match p {
                            crate::ToolContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    // Use a non-empty placeholder when the tool returned only images.
                    let output_text = if output_text.is_empty() {
                        "[see attached images]".to_string()
                    } else {
                        output_text
                    };

                    let mut result_parts: Vec<Value> = vec![json!({
                        "functionResponse": {
                            "name": fn_name,
                            "response": { "output": output_text },
                        }
                    })];
                    for p in parts {
                        if let crate::ToolContentPart::Image { image_url } = p {
                            if let Ok((mime, data)) = crate::types::parse_data_url_parts(image_url) {
                                result_parts.push(json!({
                                    "inline_data": { "mime_type": mime, "data": data }
                                }));
                            }
                        }
                    }
                    result_parts
                }
            }
        }
    }
}

fn parse_gemini_chunk(v: &Value) -> anyhow::Result<ResponseEvent> {
    // Usage metadata
    if let Some(meta) = v.get("usageMetadata") {
        // Google Gemini reports cached tokens in cachedContentTokenCount
        let cache_read_tokens = meta["cachedContentTokenCount"]
            .as_u64().unwrap_or(0) as u32;
        return Ok(ResponseEvent::Usage {
            input_tokens: meta["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            output_tokens: meta["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
            cache_read_tokens,
            cache_write_tokens: 0,
        });
    }

    let candidate = &v["candidates"][0];
    let content = &candidate["content"];
    let parts = match content["parts"].as_array() {
        Some(p) => p,
        None => {
            // End of stream signal
            if candidate["finishReason"].as_str().is_some() {
                return Ok(ResponseEvent::Done);
            }
            return Ok(ResponseEvent::TextDelta(String::new()));
        }
    };

    for part in parts {
        // Thinking / reasoning delta
        if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
            if let Some(text) = part["text"].as_str() {
                return Ok(ResponseEvent::ThinkingDelta(text.to_string()));
            }
        }
        // Function call
        if let Some(fc) = part.get("functionCall") {
            let name = fc["name"].as_str().unwrap_or("").to_string();
            let args = serde_json::to_string(&fc["args"]).unwrap_or_default();
            return Ok(ResponseEvent::ToolCall {
                id: name.clone(),
                name,
                arguments: args,
            });
        }
        // Text
        if let Some(text) = part["text"].as_str() {
            return Ok(ResponseEvent::TextDelta(text.to_string()));
        }
    }

    // finishReason present without parts → stream finished
    if candidate["finishReason"].as_str().is_some() {
        return Ok(ResponseEvent::Done);
    }

    Ok(ResponseEvent::TextDelta(String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;

    #[test]
    fn provider_name() {
        let p = GoogleProvider::new("gemini-2.0-flash-exp".into(), None, None, None, None);
        assert_eq!(p.name(), "google");
        assert_eq!(p.model_name(), "gemini-2.0-flash-exp");
    }

    #[test]
    fn usage_event_parsed() {
        let v = json!({
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50,
            }
        });
        let ev = parse_gemini_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::Usage { input_tokens: 100, output_tokens: 50, .. }));
    }

    #[test]
    fn text_delta_parsed() {
        let v = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "hello" }]
                }
            }]
        });
        let ev = parse_gemini_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "hello"));
    }

    #[test]
    fn thinking_delta_parsed() {
        let v = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "thinking...", "thought": true }]
                }
            }]
        });
        let ev = parse_gemini_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::ThinkingDelta(t) if t == "thinking..."));
    }

    #[test]
    fn function_call_parsed() {
        let v = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "shell",
                            "args": { "command": "ls" }
                        }
                    }]
                }
            }]
        });
        let ev = parse_gemini_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::ToolCall { name, .. } if name == "shell"));
    }

    // ── message_to_gemini_parts ───────────────────────────────────────────────

    #[test]
    fn tool_result_uses_function_name_not_call_id() {
        use crate::{FunctionCall, Message, MessageContent};
        // Build a conversation: ToolCall then ToolResult.
        let tc_msg = Message {
            role: crate::Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_opaque_id_123".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
        };
        let tr_msg = Message::tool_result("call_opaque_id_123", "contents");

        // Build the lookup map as `complete()` does.
        let mut tc_name_map = HashMap::new();
        if let MessageContent::ToolCall { tool_call_id, function } = &tc_msg.content {
            tc_name_map.insert(tool_call_id.clone(), function.name.clone());
        }

        let parts = message_to_gemini_parts(&tr_msg, &tc_name_map);
        assert_eq!(parts.len(), 1);
        // The functionResponse must use the function name, not the opaque ID.
        assert_eq!(parts[0]["functionResponse"]["name"], "read_file",
            "functionResponse.name must be the function name, not the call ID");
    }

    #[test]
    fn tool_result_falls_back_to_call_id_when_no_mapping() {
        use crate::Message;
        let tr_msg = Message::tool_result("unmapped_id", "result");
        let parts = message_to_gemini_parts(&tr_msg, &HashMap::new());
        assert_eq!(parts[0]["functionResponse"]["name"], "unmapped_id");
    }

    #[test]
    fn tool_result_parts_image_only_uses_placeholder_text() {
        use crate::{Message, ToolContentPart};
        let msg = Message::tool_result_with_parts("tc-1", vec![
            ToolContentPart::Image {
                image_url: "data:image/png;base64,iVBORw0KGgo=".into(),
            },
        ]);
        let parts = message_to_gemini_parts(&msg, &HashMap::new());
        // Should have functionResponse + 1 inline_data part
        assert!(parts.len() >= 1);
        let resp_output = &parts[0]["functionResponse"]["response"]["output"];
        assert_eq!(resp_output, "[see attached images]",
            "image-only tool results must use placeholder text in functionResponse");
    }

    #[test]
    fn content_parts_image_serialized_as_inline_data() {
        use crate::{ContentPart, Message};
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "look".into() },
            ContentPart::image("data:image/png;base64,abc="),
        ]);
        let parts = message_to_gemini_parts(&msg, &HashMap::new());
        assert_eq!(parts[0]["text"], "look");
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "abc=");
    }
}
