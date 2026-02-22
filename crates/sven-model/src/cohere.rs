//! Cohere driver — native Chat API v2.
//!
//! Uses the `POST /v2/chat` endpoint with streaming.
//! Cohere's wire format differs from OpenAI: different message structure,
//! different tool format, and a custom SSE event schema.

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

pub struct CohereProvider {
    model: String,
    api_key: Option<String>,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl CohereProvider {
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
            base_url: base_url.unwrap_or_else(|| "https://api.cohere.com".into()),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for CohereProvider {
    fn name(&self) -> &str { "cohere" }
    fn model_name(&self) -> &str { &self.model }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let mut entries: Vec<ModelCatalogEntry> = static_catalog()
            .into_iter()
            .filter(|e| e.provider == "cohere")
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let key = self.api_key.as_deref().context("COHERE_API_KEY not set")?;

        // Cohere v2 uses the same roles as OpenAI but different tool shapes.
        let mut system_text = String::new();
        let mut messages: Vec<Value> = Vec::new();

        for m in &req.messages {
            match m.role {
                Role::System => {
                    if let Some(t) = m.as_text() {
                        system_text = t.to_string();
                    }
                }
                _ => {
                    let role = match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "tool",
                        Role::System => unreachable!(),
                    };
                    match &m.content {
                        MessageContent::Text(t) => {
                            messages.push(json!({ "role": role, "content": t }));
                        }
                        MessageContent::ToolCall { tool_call_id, function } => {
                            messages.push(json!({
                                "role": "assistant",
                                "tool_calls": [{
                                    "id": tool_call_id,
                                    "type": "function",
                                    "function": {
                                        "name": function.name,
                                        "arguments": function.arguments,
                                    }
                                }]
                            }));
                        }
                        MessageContent::ToolResult { tool_call_id, content } => {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_call_id,
                                "content": content,
                            }));
                        }
                    }
                }
            }
        }

        let tools: Vec<Value> = req.tools.iter().map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        })).collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": req.stream,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });
        if !system_text.is_empty() {
            // Cohere v2: system message as first message with role "system"
            if let Some(msgs) = body["messages"].as_array_mut() {
                msgs.insert(0, json!({ "role": "system", "content": system_text }));
            }
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        debug!(model = %self.model, "sending Cohere request");

        let url = format!("{}/v2/chat", self.base_url.trim_end_matches('/'));
        let resp = self.client
            .post(&url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .context("Cohere request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Cohere error {status}: {text}");
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
                    // Cohere streaming uses `data:` prefix (SSE) for v2
                    let line = line.strip_prefix("data: ")?.trim();
                    if line == "[DONE]" {
                        return Some(Ok(ResponseEvent::Done));
                    }
                    let v: Value = serde_json::from_str(line).ok()?;
                    Some(parse_cohere_event(&v))
                })
                .collect();
            futures::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
}

fn parse_cohere_event(v: &Value) -> anyhow::Result<ResponseEvent> {
    let event_type = v["type"].as_str().unwrap_or("");
    match event_type {
        "content-delta" => {
            let text = v["delta"]["message"]["content"]["text"].as_str().unwrap_or("").to_string();
            Ok(ResponseEvent::TextDelta(text))
        }
        "tool-call-start" | "tool-call-delta" => {
            let tc = &v["delta"]["message"]["tool_calls"];
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
            Ok(ResponseEvent::ToolCall { id, name, arguments: args })
        }
        "message-end" => {
            if let Some(usage) = v.get("delta").and_then(|d| d.get("usage")) {
                let input_tokens = usage["billed_units"]["input_tokens"]
                    .as_u64()
                    .or_else(|| usage["tokens"]["input_tokens"].as_u64())
                    .unwrap_or(0) as u32;
                let output_tokens = usage["billed_units"]["output_tokens"]
                    .as_u64()
                    .or_else(|| usage["tokens"]["output_tokens"].as_u64())
                    .unwrap_or(0) as u32;
                return Ok(ResponseEvent::Usage { input_tokens, output_tokens });
            }
            Ok(ResponseEvent::Done)
        }
        _ => Ok(ResponseEvent::TextDelta(String::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;

    #[test]
    fn provider_name() {
        let p = CohereProvider::new("command-r-plus".into(), None, None, None, None);
        assert_eq!(p.name(), "cohere");
        assert_eq!(p.model_name(), "command-r-plus");
    }

    #[test]
    fn text_delta_parsed() {
        let v = json!({
            "type": "content-delta",
            "delta": {
                "message": {
                    "content": { "text": "hello" }
                }
            }
        });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "hello"));
    }

    #[test]
    fn message_end_returns_done_when_no_usage() {
        let v = json!({ "type": "message-end", "delta": {} });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::Done));
    }

    #[test]
    fn message_end_with_usage_yields_usage_event() {
        let v = json!({
            "type": "message-end",
            "delta": {
                "usage": {
                    "billed_units": {
                        "input_tokens": 20,
                        "output_tokens": 10
                    }
                }
            }
        });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage { input_tokens: 20, output_tokens: 10 }),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn tool_call_start_parsed() {
        let v = json!({
            "type": "tool-call-start",
            "delta": {
                "message": {
                    "tool_calls": {
                        "id": "tool_123",
                        "function": {
                            "name": "shell",
                            "arguments": ""
                        }
                    }
                }
            }
        });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { id, name, .. }
                if id == "tool_123" && name == "shell"),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn tool_call_delta_with_args() {
        let v = json!({
            "type": "tool-call-delta",
            "delta": {
                "message": {
                    "tool_calls": {
                        "id": "",
                        "function": {
                            "name": "",
                            "arguments": "{\"cmd\":\"ls\"}"
                        }
                    }
                }
            }
        });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { arguments, .. }
                if arguments == "{\"cmd\":\"ls\"}"),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn unknown_event_type_is_empty_delta() {
        let v = json!({ "type": "stream-start", "generation_id": "abc" });
        let ev = parse_cohere_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }
}
