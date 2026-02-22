use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

use crate::{
    catalog::{static_catalog, ModelCatalogEntry},
    provider::ResponseStream,
    CompletionRequest, ResponseEvent,
};

pub struct AnthropicProvider {
    model: String,
    api_key: Option<String>,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    /// Attach a `cache_control` block to the system message so Anthropic
    /// caches the prompt prefix, reducing input-token costs on repeated calls.
    cache_system_prompt: bool,
    /// Use the 1-hour extended TTL instead of the default 5-minute window.
    extended_cache_time: bool,
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
        Self::with_cache(model, api_key, base_url, max_tokens, temperature, false, false)
    }

    pub fn with_cache(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        cache_system_prompt: bool,
        extended_cache_time: bool,
    ) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".into()),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            cache_system_prompt,
            extended_cache_time,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for AnthropicProvider {
    fn name(&self) -> &str { "anthropic" }
    fn model_name(&self) -> &str { &self.model }

    /// Anthropic does not expose a public list-models endpoint with full
    /// metadata, so we return the static catalog entries for this provider.
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let mut entries: Vec<ModelCatalogEntry> = static_catalog()
            .into_iter()
            .filter(|e| e.provider == "anthropic")
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let key = self.api_key.as_deref().context("ANTHROPIC_API_KEY not set")?;

        let (system_text, messages) = build_anthropic_messages(&req.messages);

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
        if !system_text.is_empty() || req.system_dynamic_suffix.is_some() {
            if self.cache_system_prompt {
                // Build an array of system content blocks.
                //
                // Block 1 — stable prefix WITH cache_control (gets cached).
                //   • Default (5-min TTL): {"type": "ephemeral"} – no ttl field.
                //   • Extended (1-hour TTL): {"type": "ephemeral", "ttl": "1h"}.
                // Block 2 — volatile context WITHOUT cache_control (not cached).
                //   Git/CI info that changes between sessions lives here so the
                //   stable prefix can be reused across different sessions.
                let cache_control = if self.extended_cache_time {
                    json!({ "type": "ephemeral", "ttl": "1h" })
                } else {
                    json!({ "type": "ephemeral" })
                };

                let mut system_blocks: Vec<Value> = Vec::new();
                if !system_text.is_empty() {
                    system_blocks.push(json!({
                        "type": "text",
                        "text": system_text,
                        "cache_control": cache_control,
                    }));
                }
                // Dynamic context (git branch/commit, CI env) in a second block
                // without cache_control so it does not pollute the cached prefix.
                if let Some(dynamic) = &req.system_dynamic_suffix {
                    if !dynamic.trim().is_empty() {
                        system_blocks.push(json!({
                            "type": "text",
                            "text": dynamic,
                        }));
                    }
                }
                if !system_blocks.is_empty() {
                    body["system"] = json!(system_blocks);
                }
            } else {
                // Caching disabled: merge dynamic suffix into system text.
                let combined = match &req.system_dynamic_suffix {
                    Some(d) if !d.trim().is_empty() => {
                        format!("{}\n\n{}", system_text, d)
                    }
                    _ => system_text,
                };
                if !combined.is_empty() {
                    body["system"] = json!(combined);
                }
            }
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        debug!(
            model = %self.model,
            cache_system_prompt = self.cache_system_prompt,
            "sending anthropic request",
        );

        let mut request_builder = self.client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");

        // Prompt caching requires the beta header for Claude 3 / 3.5 Sonnet
        // (original).  It is safe to send it unconditionally for all claude-3+
        // models; newer models ignore it.
        if self.cache_system_prompt {
            request_builder = request_builder
                .header("anthropic-beta", "prompt-caching-2024-07-31");
        }

        let resp = request_builder
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

pub(crate) fn parse_anthropic_event(v: &Value) -> anyhow::Result<ResponseEvent> {
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
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                });
            }
            Ok(ResponseEvent::TextDelta(String::new()))
        }
        "message_start" => {
            if let Some(usage) = v["message"].get("usage") {
                return Ok(ResponseEvent::Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0) as u32,
                    output_tokens: 0,
                    // Anthropic reports these only in message_start.
                    cache_read_tokens: usage["cache_read_input_tokens"]
                        .as_u64().unwrap_or(0) as u32,
                    cache_write_tokens: usage["cache_creation_input_tokens"]
                        .as_u64().unwrap_or(0) as u32,
                });
            }
            Ok(ResponseEvent::TextDelta(String::new()))
        }
        "message_stop" => Ok(ResponseEvent::Done),
        _ => Ok(ResponseEvent::TextDelta(String::new())),
    }
}

/// Convert a slice of [`Message`]s into the Anthropic wire format.
///
/// Returns `(system_text, conversation_messages)`.  The system message is
/// separated out because Anthropic expects it as a top-level `system` field,
/// not as a conversation turn.
pub(crate) fn build_anthropic_messages(
    messages: &[crate::Message],
) -> (String, Vec<Value>) {
    use crate::{ContentPart, MessageContent, Role, ToolContentPart, ToolResultContent};

    let mut system_text = String::new();
    let mut out: Vec<Value> = Vec::new();

    for m in messages {
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
                out.push(json!({ "role": role, "content": t }));
            }
            MessageContent::ContentParts(parts) if !parts.is_empty() => {
                let content: Vec<Value> = parts.iter().map(|p| match p {
                    ContentPart::Text { text } => {
                        json!({ "type": "text", "text": text })
                    }
                    ContentPart::Image { image_url, .. } => {
                        if let Ok((mime, data)) = crate::types::parse_data_url_parts(image_url) {
                            json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mime,
                                    "data": data,
                                }
                            })
                        } else {
                            json!({
                                "type": "image",
                                "source": { "type": "url", "url": image_url }
                            })
                        }
                    }
                }).collect();
                out.push(json!({ "role": role, "content": content }));
            }
            MessageContent::ContentParts(_) => {
                out.push(json!({ "role": role, "content": "" }));
            }
            MessageContent::ToolCall { tool_call_id, function } => {
                out.push(json!({
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
                let wire_content: Value = match content {
                    ToolResultContent::Text(t) => json!(t),
                    ToolResultContent::Parts(parts) if !parts.is_empty() => {
                        let arr: Vec<Value> = parts.iter().map(|p| match p {
                            ToolContentPart::Text { text } => {
                                json!({ "type": "text", "text": text })
                            }
                            ToolContentPart::Image { image_url } => {
                                if let Ok((mime, data)) = crate::types::parse_data_url_parts(image_url) {
                                    json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": mime,
                                            "data": data,
                                        }
                                    })
                                } else {
                                    json!({
                                        "type": "image",
                                        "source": { "type": "url", "url": image_url }
                                    })
                                }
                            }
                        }).collect();
                        json!(arr)
                    }
                    ToolResultContent::Parts(_) => json!(""),
                };
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": wire_content,
                    }]
                }));
            }
        }
    }
    (system_text, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;

    #[test]
    fn provider_name_and_model() {
        let p = AnthropicProvider::new(
            "claude-3-5-sonnet-20241022".into(), None, None, None, None,
        );
        assert_eq!(p.name(), "anthropic");
        assert_eq!(p.model_name(), "claude-3-5-sonnet-20241022");
    }

    // ── parse_anthropic_event ─────────────────────────────────────────────────

    #[test]
    fn message_start_yields_input_usage() {
        let v = serde_json::json!({
            "type": "message_start",
            "message": {
                "usage": { "input_tokens": 42, "output_tokens": 0 }
            }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage { input_tokens: 42, output_tokens: 0, .. }),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn message_start_parses_cache_tokens() {
        let v = serde_json::json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 20
                }
            }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage {
                input_tokens: 100,
                cache_read_tokens: 80,
                cache_write_tokens: 20,
                ..
            }),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn message_start_without_usage_is_empty_delta() {
        let v = serde_json::json!({ "type": "message_start", "message": {} });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn content_block_start_tool_use_emits_tool_call() {
        let v = serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "tool_use", "id": "toolu_01", "name": "shell" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { id, name, arguments }
                if id == "toolu_01" && name == "shell" && arguments.is_empty()),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn content_block_start_text_is_empty_delta() {
        let v = serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text", "text": "" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn content_block_delta_text_delta() {
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "world" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "world"));
    }

    #[test]
    fn content_block_delta_input_json_delta() {
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "input_json_delta", "partial_json": "{\"key\":" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { arguments, .. } if arguments == "{\"key\":"),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn content_block_delta_unknown_type_is_empty_delta() {
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "thinking_delta", "thinking": "hmm" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn message_delta_yields_output_usage() {
        let v = serde_json::json!({
            "type": "message_delta",
            "usage": { "output_tokens": 88 }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage { input_tokens: 0, output_tokens: 88, .. }),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn message_delta_without_usage_is_empty_delta() {
        let v = serde_json::json!({ "type": "message_delta", "delta": {} });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn message_stop_yields_done() {
        let v = serde_json::json!({ "type": "message_stop" });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::Done));
    }

    #[test]
    fn unknown_event_type_is_empty_delta() {
        let v = serde_json::json!({ "type": "ping" });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    // ── Multimodal message serialization ────────────────────────────────────────

    #[test]
    fn plain_text_message_serialized_correctly() {
        use crate::Message;
        let (sys, msgs) = build_anthropic_messages(&[Message::user("hello")]);
        assert!(sys.is_empty());
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hello");
    }

    #[test]
    fn system_message_extracted_to_system_text() {
        use crate::Message;
        let (sys, msgs) = build_anthropic_messages(&[
            Message::system("be helpful"),
            Message::user("hi"),
        ]);
        assert_eq!(sys, "be helpful");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn content_parts_image_base64_uses_source_block() {
        use crate::{ContentPart, Message};
        let data_url = "data:image/png;base64,iVBORw0KGgo=";
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "look at this".into() },
            ContentPart::image(data_url),
        ]);
        let (_, msgs) = build_anthropic_messages(&[msg]);
        let content = &msgs[0]["content"];
        assert_eq!(content[0]["type"], "text");
        let img = &content[1];
        assert_eq!(img["type"], "image");
        assert_eq!(img["source"]["type"], "base64");
        assert_eq!(img["source"]["media_type"], "image/png");
        assert_eq!(img["source"]["data"], "iVBORw0KGgo=");
    }

    #[test]
    fn content_parts_image_https_url_uses_url_source() {
        use crate::{ContentPart, Message};
        let url = "https://example.com/img.jpg";
        let msg = Message::user_with_parts(vec![
            ContentPart::image(url),
        ]);
        let (_, msgs) = build_anthropic_messages(&[msg]);
        let img = &msgs[0]["content"][0];
        assert_eq!(img["source"]["type"], "url");
        assert_eq!(img["source"]["url"], url);
    }

    #[test]
    fn tool_result_parts_with_image_serialized_as_tool_result_content_array() {
        use crate::{Message, ToolContentPart};
        let data_url = "data:image/jpeg;base64,/9j/4AAQ=";
        let msg = Message::tool_result_with_parts("tc-42", vec![
            ToolContentPart::Text { text: "screenshot".into() },
            ToolContentPart::Image { image_url: data_url.into() },
        ]);
        let (_, msgs) = build_anthropic_messages(&[msg]);
        assert_eq!(msgs[0]["role"], "user");
        let block = &msgs[0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "tc-42");
        let content = &block["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        let img = &content[1];
        assert_eq!(img["type"], "image");
        assert_eq!(img["source"]["type"], "base64");
    }
}
