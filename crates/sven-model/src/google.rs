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

        // Separate system instruction from conversation
        let mut system_parts: Vec<Value> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for m in &req.messages {
            match m.role {
                Role::System => {
                    if let Some(t) = m.as_text() {
                        system_parts.push(json!({ "text": t }));
                    }
                }
                Role::User | Role::Tool => {
                    let parts = message_to_gemini_parts(m);
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
                Role::Assistant => {
                    let parts = message_to_gemini_parts(m);
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
fn message_to_gemini_parts(m: &crate::Message) -> Vec<Value> {
    match &m.content {
        MessageContent::Text(t) => vec![json!({ "text": t })],
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
            vec![json!({
                "functionResponse": {
                    "name": tool_call_id,
                    "response": { "output": content },
                }
            })]
        }
    }
}

fn parse_gemini_chunk(v: &Value) -> anyhow::Result<ResponseEvent> {
    // Usage metadata
    if let Some(meta) = v.get("usageMetadata") {
        return Ok(ResponseEvent::Usage {
            input_tokens: meta["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            output_tokens: meta["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
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
        assert!(matches!(ev, ResponseEvent::Usage { input_tokens: 100, output_tokens: 50 }));
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
}
