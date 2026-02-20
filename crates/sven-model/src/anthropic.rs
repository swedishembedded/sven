use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

use crate::{provider::ResponseStream, CompletionRequest, MessageContent, ResponseEvent, Role};

pub struct AnthropicProvider {
    model: String,
    api_key: Option<String>,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl AnthropicProvider {
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
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".into()),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for AnthropicProvider {
    fn name(&self) -> &str { "anthropic" }
    fn model_name(&self) -> &str { &self.model }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let key = self.api_key.as_deref().context("ANTHROPIC_API_KEY not set")?;

        // Separate system message from conversation
        let mut system_text = String::new();
        let mut messages: Vec<Value> = Vec::new();

        for m in &req.messages {
            if m.role == Role::System {
                if let Some(t) = m.as_text() {
                    system_text = t.to_string();
                }
                continue;
            }
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
                Role::System => unreachable!(),
            };
            match &m.content {
                MessageContent::Text(t) => {
                    messages.push(json!({ "role": role, "content": t }));
                }
                MessageContent::ToolCall { tool_call_id, function } => {
                    messages.push(json!({
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use",
                            "id": tool_call_id,
                            "name": function.name,
                            "input": serde_json::from_str::<Value>(&function.arguments)
                                .unwrap_or(json!({})),
                        }]
                    }));
                }
                MessageContent::ToolResult { tool_call_id, content } => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content,
                        }]
                    }));
                }
            }
        }

        let tools: Vec<Value> = req.tools.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.parameters,
        })).collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "stream": req.stream,
        });
        if !system_text.is_empty() {
            body["system"] = json!(system_text);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        debug!(model = %self.model, "sending anthropic request");

        let resp = self.client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("Anthropic request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic error {status}: {text}");
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
                    let v: Value = serde_json::from_str(line).ok()?;
                    Some(parse_anthropic_event(&v))
                })
                .collect();
            futures::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
}

fn parse_anthropic_event(v: &Value) -> anyhow::Result<ResponseEvent> {
    let event_type = v["type"].as_str().unwrap_or("");
    match event_type {
        "content_block_delta" => {
            let delta = &v["delta"];
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    let text = delta["text"].as_str().unwrap_or("").to_string();
                    Ok(ResponseEvent::TextDelta(text))
                }
                "input_json_delta" => {
                    let partial = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Ok(ResponseEvent::ToolCall { id: String::new(), name: String::new(), arguments: partial })
                }
                _ => Ok(ResponseEvent::TextDelta(String::new())),
            }
        }
        "content_block_start" => {
            let block = &v["content_block"];
            if block["type"].as_str() == Some("tool_use") {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                Ok(ResponseEvent::ToolCall { id, name, arguments: String::new() })
            } else {
                Ok(ResponseEvent::TextDelta(String::new()))
            }
        }
        "message_delta" => {
            if let Some(usage) = v.get("usage") {
                return Ok(ResponseEvent::Usage {
                    input_tokens: 0,
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0) as u32,
                });
            }
            Ok(ResponseEvent::TextDelta(String::new()))
        }
        "message_start" => {
            if let Some(usage) = v["message"].get("usage") {
                return Ok(ResponseEvent::Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0) as u32,
                    output_tokens: 0,
                });
            }
            Ok(ResponseEvent::TextDelta(String::new()))
        }
        "message_stop" => Ok(ResponseEvent::Done),
        _ => Ok(ResponseEvent::TextDelta(String::new())),
    }
}
