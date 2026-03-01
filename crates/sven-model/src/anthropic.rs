// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::{debug, warn};

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
    /// Applies to the system prompt and tool definitions when caching is enabled.
    extended_cache_time: bool,
    /// Attach a `cache_control` marker to the last tool definition so all tool
    /// definitions are cached as a single prefix.
    cache_tools: bool,
    /// Add a top-level `cache_control` to enable automatic conversation caching.
    /// Anthropic automatically moves the cache breakpoint forward with each turn.
    cache_conversation: bool,
    /// Mark the oldest image content blocks in conversation history with
    /// `cache_control` so Anthropic caches them.  Images cost hundreds of
    /// tokens even when small; caching them once saves ~90% on every
    /// subsequent turn they remain in context.
    cache_images: bool,
    /// Mark large tool result blocks (>= TOOL_RESULT_CACHE_CHARS) in
    /// conversation history with `cache_control`.  File reads and command
    /// outputs that persist across many turns are ideal candidates.
    cache_tool_results: bool,
    client: reqwest::Client,
}

/// Minimum serialised content length (in bytes) for a tool result to be
/// eligible for explicit caching.  Matches Anthropic's minimum cacheable
/// prompt length for Sonnet-class models (~1 024 tokens × 4 chars/token).
const TOOL_RESULT_CACHE_CHARS: usize = 4096;

impl AnthropicProvider {
    pub fn new(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        Self::with_cache(
            model,
            api_key,
            base_url,
            max_tokens,
            temperature,
            false,
            false,
            false,
            false,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_cache(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        cache_system_prompt: bool,
        extended_cache_time: bool,
        cache_tools: bool,
        cache_conversation: bool,
        cache_images: bool,
        cache_tool_results: bool,
    ) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".into()),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            cache_system_prompt,
            extended_cache_time,
            cache_tools,
            cache_conversation,
            cache_images,
            cache_tool_results,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn model_name(&self) -> &str {
        &self.model
    }

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
        let key = self
            .api_key
            .as_deref()
            .context("ANTHROPIC_API_KEY not set")?;

        let (system_text, mut messages) = build_anthropic_messages(&req.messages);

        // Build the TTL-appropriate cache_control object.
        // Tools and system prompt share the same TTL tier so the ordering
        // constraint (longer TTL must precede shorter TTL) is always satisfied.
        let cache_ctrl = if self.extended_cache_time {
            json!({ "type": "ephemeral", "ttl": "1h" })
        } else {
            json!({ "type": "ephemeral" })
        };

        // ── Per-block history caching ────────────────────────────────────────
        // Anthropic allows up to 4 cache breakpoints per request.  After
        // allocating slots for system prompt, tools, and automatic conversation
        // caching, any remaining slots can be used to cache expensive blocks
        // that persist across many turns:
        //
        //   • Images     — hundreds of tokens each; stable once uploaded.
        //   • Large tool results — file reads/command outputs that linger in
        //     context for many turns after the tool call that produced them.
        //
        // We walk the messages array FORWARD (oldest first) so that the oldest
        // stable content gets cached first — it will be present the longest and
        // therefore yield the most cache hits.
        //
        // TTL ordering is preserved: images and tool results receive the same
        // TTL tier as system/tools (`cache_ctrl`), which is always ≥ the 5-min
        // TTL used by automatic conversation caching.
        let slots_used =
            self.cache_system_prompt as u8 + self.cache_tools as u8 + self.cache_conversation as u8;
        let avail = 4u8.saturating_sub(slots_used);

        if avail > 0 && (self.cache_images || self.cache_tool_results) {
            let mut added = 0u8;
            'outer: for msg in messages.iter_mut() {
                if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                    for block in content.iter_mut() {
                        if added >= avail {
                            break 'outer;
                        }
                        let btype = block["type"].as_str().unwrap_or("");
                        let should_cache = (self.cache_images
                            && btype == "image"
                            && block.get("cache_control").is_none())
                            || (self.cache_tool_results
                                && btype == "tool_result"
                                && block.get("cache_control").is_none()
                                && block["content"].to_string().len() >= TOOL_RESULT_CACHE_CHARS);
                        if should_cache {
                            block["cache_control"] = cache_ctrl.clone();
                            added += 1;
                        }
                    }
                }
            }
        }

        // Tools — optionally mark the last definition with cache_control so
        // Anthropic caches the entire tools array as a prefix.
        let tools: Vec<Value> = if !req.tools.is_empty() && self.cache_tools {
            let last = req.tools.len() - 1;
            req.tools
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if i == last {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                            "cache_control": cache_ctrl,
                        })
                    } else {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                        })
                    }
                })
                .collect()
        } else {
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect()
        };

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "stream": req.stream,
        });

        // Automatic conversation caching — add a top-level cache_control block.
        // Anthropic automatically moves the breakpoint to the last cacheable
        // block on each turn, so the growing conversation history is cached
        // incrementally with no per-message bookkeeping.
        if self.cache_conversation {
            body["cache_control"] = json!({ "type": "ephemeral" });
        }

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
                let mut system_blocks: Vec<Value> = Vec::new();
                if !system_text.is_empty() {
                    system_blocks.push(json!({
                        "type": "text",
                        "text": system_text,
                        "cache_control": cache_ctrl,
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

        let any_caching = self.cache_system_prompt
            || self.cache_tools
            || self.cache_conversation
            || self.cache_images
            || self.cache_tool_results;
        debug!(
            model = %self.model,
            cache_system_prompt = self.cache_system_prompt,
            cache_tools = self.cache_tools,
            cache_conversation = self.cache_conversation,
            cache_images = self.cache_images,
            cache_tool_results = self.cache_tool_results,
            extended_cache_time = self.extended_cache_time,
            "sending anthropic request",
        );

        let mut request_builder = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");

        // Build the anthropic-beta header.
        //
        // • `prompt-caching-2024-07-31` — required for prompt caching on older
        //   Claude 3 / 3.5 Sonnet models.  Safe to send for all claude-3+ models;
        //   newer models silently ignore it.
        // • `extended-cache-ttl-2025-04-11` — required when using 1-hour TTL.
        //
        // Multiple beta features are enabled via a comma-separated value.
        if any_caching {
            let mut betas: Vec<&str> = vec!["prompt-caching-2024-07-31"];
            if self.extended_cache_time {
                betas.push("extended-cache-ttl-2025-04-11");
            }
            request_builder = request_builder.header("anthropic-beta", betas.join(","));
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
        // SSE lines can be split across TCP chunks, so we carry a remainder
        // buffer forward.  Only complete lines (terminated by '\n') are parsed;
        // anything left over is prepended to the next chunk.
        let event_stream = byte_stream
            .scan(String::new(), |buf, chunk| {
                let text = match chunk {
                    Ok(b) => String::from_utf8_lossy(&b).to_string(),
                    Err(e) => {
                        return futures::future::ready(Some(vec![Err(anyhow::anyhow!(e))]));
                    }
                };
                buf.push_str(&text);
                let mut events = Vec::new();
                // Process every complete line (i.e. everything before the last '\n').
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim_end_matches('\r').to_string();
                    buf.drain(..=pos);
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            events.push(parse_anthropic_event(&v));
                        }
                    }
                }
                futures::future::ready(Some(events))
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(event_stream))
    }
}

pub(crate) fn parse_anthropic_event(v: &Value) -> anyhow::Result<ResponseEvent> {
    let event_type = v["type"].as_str().unwrap_or("");
    match event_type {
        "content_block_delta" => {
            let index = v["index"].as_u64().unwrap_or(0) as u32;
            let delta = &v["delta"];
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    let text = delta["text"].as_str().unwrap_or("").to_string();
                    Ok(ResponseEvent::TextDelta(text))
                }
                "input_json_delta" => {
                    let partial = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Ok(ResponseEvent::ToolCall {
                        index,
                        id: String::new(),
                        name: String::new(),
                        arguments: partial,
                    })
                }
                // Extended thinking: Claude streams the chain-of-thought as a
                // separate delta type.  Map it to ThinkingDelta so the CI runner
                // and TUI can surface it without mixing it into the answer text.
                "thinking_delta" => {
                    let thinking = delta["thinking"].as_str().unwrap_or("").to_string();
                    if thinking.is_empty() {
                        Ok(ResponseEvent::TextDelta(String::new()))
                    } else {
                        Ok(ResponseEvent::ThinkingDelta(thinking))
                    }
                }
                // Anthropic sends an encrypted signature blob at the end of every
                // thinking block so the server can verify integrity.  It is not
                // human-readable and must never be shown or logged as plain text.
                "signature_delta" => Ok(ResponseEvent::TextDelta(String::new())),
                _ => Ok(ResponseEvent::TextDelta(String::new())),
            }
        }
        "content_block_start" => {
            let index = v["index"].as_u64().unwrap_or(0) as u32;
            let block = &v["content_block"];
            if block["type"].as_str() == Some("tool_use") {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                Ok(ResponseEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments: String::new(),
                })
            } else {
                Ok(ResponseEvent::TextDelta(String::new()))
            }
        }
        "message_delta" => {
            // Anthropic reports the final stop_reason in delta.stop_reason.
            // When the model hit the output-token limit, emit MaxTokens so
            // the agent knows any in-flight tool-call arguments were truncated.
            // We prioritise this over the accompanying usage data; the output
            // token count for a truncated turn is necessarily max_output_tokens
            // so the slight under-count in token tracking is acceptable.
            if v["delta"]["stop_reason"].as_str() == Some("max_tokens") {
                return Ok(ResponseEvent::MaxTokens);
            }
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
                    cache_read_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        as u32,
                    cache_write_tokens: usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                        as u32,
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
pub(crate) fn build_anthropic_messages(messages: &[crate::Message]) -> (String, Vec<Value>) {
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
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => {
                            json!({ "type": "text", "text": text })
                        }
                        ContentPart::Image { image_url, .. } => {
                            if let Ok((mime, data)) = crate::types::parse_data_url_parts(image_url)
                            {
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
                    })
                    .collect();
                out.push(json!({ "role": role, "content": content }));
            }
            MessageContent::ContentParts(_) => {
                out.push(json!({ "role": role, "content": "" }));
            }
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => {
                // Anthropic requires tool_use.id to match `^[a-zA-Z0-9_-]+$`.
                // An empty id can arise when a content_block_start event was
                // missing from the stream.  Rather than sending an invalid
                // request (which yields a 400), use a stable fallback so the
                // conversation remains coherent.
                let safe_id = if tool_call_id.is_empty() {
                    warn!(
                        tool_name = %function.name,
                        "ToolCall message has empty tool_call_id when building \
                         Anthropic request; substituting fallback id"
                    );
                    "tc_fallback".to_string()
                } else {
                    tool_call_id.clone()
                };
                out.push(json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": safe_id,
                        "name": function.name,
                        "input": serde_json::from_str::<Value>(&function.arguments)
                            .unwrap_or(json!({})),
                    }]
                }));
            }
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => {
                let wire_content: Value = match content {
                    ToolResultContent::Text(t) => json!(t),
                    ToolResultContent::Parts(parts) if !parts.is_empty() => {
                        let arr: Vec<Value> = parts
                            .iter()
                            .map(|p| match p {
                                ToolContentPart::Text { text } => {
                                    json!({ "type": "text", "text": text })
                                }
                                ToolContentPart::Image { image_url } => {
                                    if let Ok((mime, data)) =
                                        crate::types::parse_data_url_parts(image_url)
                                    {
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
                            })
                            .collect();
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
        let p = AnthropicProvider::new("claude-3-5-sonnet-20241022".into(), None, None, None, None);
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
            matches!(
                ev,
                ResponseEvent::Usage {
                    input_tokens: 42,
                    output_tokens: 0,
                    ..
                }
            ),
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
            matches!(
                ev,
                ResponseEvent::Usage {
                    input_tokens: 100,
                    cache_read_tokens: 80,
                    cache_write_tokens: 20,
                    ..
                }
            ),
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
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_01", "name": "shell" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { index, id, name, arguments }
                if *index == 0 && id == "toolu_01" && name == "shell" && arguments.is_empty()),
            "unexpected: {ev:?}"
        );
    }

    #[test]
    fn content_block_start_tool_use_preserves_index() {
        let v = serde_json::json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": { "type": "tool_use", "id": "toolu_02", "name": "read_file" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { index, .. } if *index == 2),
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
    fn content_block_delta_thinking_delta_produces_thinking_event() {
        // Extended thinking: Claude emits `thinking_delta` blocks with the CoT text.
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "thinking_delta", "thinking": "Let me reason through this." }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ThinkingDelta(t) if t == "Let me reason through this."),
            "expected ThinkingDelta, got {ev:?}"
        );
    }

    #[test]
    fn content_block_delta_thinking_delta_empty_is_empty_text_delta() {
        // An empty thinking delta should not produce a ThinkingDelta event.
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "thinking_delta", "thinking": "" }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn content_block_delta_signature_delta_is_silently_discarded() {
        // Anthropic sends an encrypted `signature_delta` at the end of each
        // thinking block.  It must never be emitted as readable text or thinking.
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "signature_delta", "signature": "EqRkLm..." }
        });
        let ev = parse_anthropic_event(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::TextDelta(ref t) if t.is_empty()),
            "signature_delta must be silently discarded, got {ev:?}"
        );
    }

    #[test]
    fn content_block_delta_unknown_type_is_empty_delta() {
        // Any other unknown delta type should produce an empty TextDelta.
        let v = serde_json::json!({
            "type": "content_block_delta",
            "delta": { "type": "some_future_type", "data": "xyz" }
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
            matches!(
                ev,
                ResponseEvent::Usage {
                    input_tokens: 0,
                    output_tokens: 88,
                    ..
                }
            ),
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
        let (sys, msgs) =
            build_anthropic_messages(&[Message::system("be helpful"), Message::user("hi")]);
        assert_eq!(sys, "be helpful");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn content_parts_image_base64_uses_source_block() {
        use crate::{ContentPart, Message};
        let data_url = "data:image/png;base64,iVBORw0KGgo=";
        let msg = Message::user_with_parts(vec![
            ContentPart::Text {
                text: "look at this".into(),
            },
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
        let msg = Message::user_with_parts(vec![ContentPart::image(url)]);
        let (_, msgs) = build_anthropic_messages(&[msg]);
        let img = &msgs[0]["content"][0];
        assert_eq!(img["source"]["type"], "url");
        assert_eq!(img["source"]["url"], url);
    }

    #[test]
    fn tool_result_parts_with_image_serialized_as_tool_result_content_array() {
        use crate::{Message, ToolContentPart};
        let data_url = "data:image/jpeg;base64,/9j/4AAQ=";
        let msg = Message::tool_result_with_parts(
            "tc-42",
            vec![
                ToolContentPart::Text {
                    text: "screenshot".into(),
                },
                ToolContentPart::Image {
                    image_url: data_url.into(),
                },
            ],
        );
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
