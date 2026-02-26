// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Shared base implementation for OpenAI-compatible chat completion APIs.
//!
//! Roughly 25 providers speak the same `/chat/completions` + `/models` wire
//! format.  This module provides a single `OpenAICompatProvider` that every
//! such driver configures with its own defaults (URL, auth style, headers).
//!
//! # Auth styles
//! - `Bearer` — `Authorization: Bearer <key>` (most providers)
//! - `ApiKeyHeader` — `api-key: <key>` (Azure OpenAI)
//! - `None` — no authentication (local servers like Ollama / LM Studio)
//!
//! # Usage
//! Configure via `sven_config::ModelConfig` and call `sven_model::from_config`.
//! This module is `pub(crate)` — direct construction is handled in
//! `sven_model::from_config`.

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

use crate::{
    catalog::{static_catalog, ModelCatalogEntry},
    provider::ResponseStream,
    CompletionRequest, ResponseEvent, Role,
};

/// How to send the API key in HTTP requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` — standard for most providers.
    Bearer,
    /// `api-key: <key>` — Azure OpenAI style.
    ApiKeyHeader,
    /// No authentication header — local servers (Ollama, vLLM, LM Studio).
    None,
}

/// OpenAI-compatible chat completion provider.
///
/// Used as the implementation for every provider that speaks the standard
/// `/v1/chat/completions` SSE streaming wire format.
pub struct OpenAICompatProvider {
    /// Provider id returned by `ModelProvider::name()`.
    driver_name: &'static str,
    /// Model id forwarded to the API.
    model: String,
    /// API key (pre-resolved from config or env).
    api_key: Option<String>,
    /// Full chat completions URL, e.g. `https://api.groq.com/openai/v1/chat/completions`.
    chat_url: String,
    /// Full models list URL (optional).  `None` → fall back to static catalog.
    models_url: Option<String>,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
    /// Additional HTTP headers (e.g. `HTTP-Referer` for OpenRouter).
    extra_headers: Vec<(String, String)>,
    auth_style: AuthStyle,
    /// Extra key-value pairs merged verbatim into the request body.
    ///
    /// Populated from `ModelConfig.driver_options`.  Use this to pass
    /// provider-specific parameters that sven does not model natively, e.g.:
    ///   • `parse_tool_calls: false` — disable llama.cpp grammar constraints
    ///     so the model can emit reasoning text alongside tool calls
    ///   • `reasoning_format: "deepseek"` — enable thinking extraction on
    ///     llama.cpp for reasoning-capable models (QwQ, DeepSeek-R1, Qwen3)
    extra_body: serde_json::Value,
}

impl OpenAICompatProvider {
    /// Construct a provider from its full endpoint URLs and auth configuration.
    ///
    /// # Parameters
    /// - `driver_name` — stable id from the registry (e.g. `"groq"`)
    /// - `model` — model identifier forwarded to the API
    /// - `api_key` — pre-resolved key (may be `None` for local servers)
    /// - `base_url` — API base that ends **before** `/chat/completions`, e.g.
    ///   `https://api.groq.com/openai/v1`
    /// - `max_tokens` — `None` uses the catalog default or 4096
    /// - `temperature` — `None` defaults to 0.2
    /// - `extra_headers` — additional `(name, value)` pairs sent on every request
    /// - `auth_style` — how the key is attached to requests
    /// - `extra_body` — JSON object merged verbatim into the request body
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        driver_name: &'static str,
        model: String,
        api_key: Option<String>,
        base_url: &str,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        extra_headers: Vec<(String, String)>,
        auth_style: AuthStyle,
        extra_body: serde_json::Value,
    ) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            driver_name,
            model,
            api_key,
            chat_url: format!("{base}/chat/completions"),
            models_url: Some(format!("{base}/models")),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
            extra_headers,
            auth_style,
            extra_body,
        }
    }

    /// Construct a provider from a **pre-built** chat completions URL.
    ///
    /// Use this when the full URL cannot be derived by appending
    /// `/chat/completions` to a base — e.g. Azure OpenAI, which encodes the
    /// deployment name and API version as path/query segments:
    /// `https://<resource>.openai.azure.com/openai/deployments/<deployment>/chat/completions?api-version=…`
    ///
    /// No `/models` URL is configured; the static catalog is used for model
    /// discovery.
    #[allow(clippy::too_many_arguments)]
    pub fn with_full_chat_url(
        driver_name: &'static str,
        model: String,
        api_key: Option<String>,
        chat_url: impl Into<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        extra_headers: Vec<(String, String)>,
        auth_style: AuthStyle,
        extra_body: serde_json::Value,
    ) -> Self {
        Self {
            driver_name,
            model,
            api_key,
            chat_url: chat_url.into(),
            models_url: None,
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
            extra_headers,
            auth_style,
            extra_body,
        }
    }
}

#[async_trait]
impl crate::ModelProvider for OpenAICompatProvider {
    fn name(&self) -> &str {
        self.driver_name
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    /// List models via `GET /models`, enriched with static catalog metadata.
    /// Falls back to catalog-only when no API key is present or the endpoint
    /// is unavailable.
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let catalog_entries: Vec<ModelCatalogEntry> = static_catalog()
            .into_iter()
            .filter(|e| e.provider == self.driver_name)
            .collect();

        let url = match &self.models_url {
            Some(u) => u.clone(),
            None => return Ok(catalog_entries),
        };

        let key = match &self.api_key {
            Some(k) => k.clone(),
            None => {
                // Local provider with no key — just return catalog.
                return Ok(catalog_entries);
            }
        };

        let mut req = self.client.get(&url);
        req = match self.auth_style {
            AuthStyle::Bearer => req.bearer_auth(&key),
            AuthStyle::ApiKeyHeader => req.header("api-key", &key),
            AuthStyle::None => req,
        };
        for (name, val) in &self.extra_headers {
            req = req.header(name.as_str(), val.as_str());
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(_) => {
                // Network error (e.g. local server not running) – return catalog.
                return Ok(catalog_entries);
            }
        };

        if !resp.status().is_success() {
            // Non-critical – return static catalog.
            return Ok(catalog_entries);
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return Ok(catalog_entries),
        };

        let mut entries: Vec<ModelCatalogEntry> = Vec::new();
        if let Some(data) = body["data"].as_array() {
            for item in data {
                let id = match item["id"].as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                // Enrich with static catalog if available.
                if let Some(cat) = catalog_entries.iter().find(|e| e.id == id) {
                    entries.push(cat.clone());
                } else {
                    entries.push(ModelCatalogEntry {
                        id: id.clone(),
                        name: id.clone(),
                        provider: self.driver_name.to_string(),
                        context_window: 0,
                        max_output_tokens: 0,
                        description: String::new(),
                        // Unknown model: conservative default (text only).
                        input_modalities: vec![crate::catalog::InputModality::Text],
                    });
                }
            }
        }

        if entries.is_empty() {
            return Ok(catalog_entries);
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        // Merge dynamic suffix into the system message before serialization.
        // OpenAI has a single "system" message; there is no separate uncached
        // block concept, so we simply append the volatile context to the text.
        let messages: Vec<Value> = if let Some(suffix) = &req.system_dynamic_suffix {
            let mut msgs = req.messages.clone();
            if let Some(sys) = msgs.first_mut() {
                if sys.role == crate::Role::System {
                    use crate::MessageContent;
                    if let MessageContent::Text(t) = &sys.content {
                        let combined = format!("{t}\n\n{suffix}");
                        sys.content = MessageContent::Text(combined);
                    }
                }
            }
            build_openai_messages(&msgs)
        } else {
            build_openai_messages(&req.messages)
        };

        let tools: Vec<Value> = req.tools.iter().map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        })).collect();

        // OpenAI's API now uses "max_completion_tokens" for newer models (gpt-5, o1, etc.)
        // Other providers still use "max_tokens"
        let max_tokens_key = if self.driver_name == "openai" {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        
        // GPT-5 models only support temperature=1 (the default)
        // Reasoning models (o1, o3) don't support temperature parameter at all
        let use_temperature = if self.driver_name == "openai" {
            !(self.model.starts_with("o1-") 
                || self.model.starts_with("o3-")
                || self.model.starts_with("gpt-5"))
        } else {
            true
        };
        
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": req.stream,
            max_tokens_key: self.max_tokens,
            "stream_options": { "include_usage": true },
        });
        if use_temperature {
            body["temperature"] = json!(self.temperature);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        // OpenRouter supports a `prompt_cache_key` body field that pins all
        // requests sharing the same key to the same cached KV prefix.  Using
        // the session ID ensures every turn within a session benefits from the
        // cached system prompt + stable conversation prefix even across
        // requests that would otherwise be treated as independent by the
        // gateway.  Other providers that speak the same field (e.g. Venice)
        // also benefit automatically.
        if self.driver_name == "openrouter" {
            if let Some(key) = &req.cache_key {
                body["prompt_cache_key"] = json!(key);
            }
        }

        // Merge driver_options (extra_body) into the request.  Keys from the
        // user-supplied JSON object override anything sven set above, so users
        // can fine-tune provider-specific behaviour without code changes:
        //
        //   • `parse_tool_calls: false`      – disable llama.cpp grammar so
        //                                      the model can emit reasoning
        //                                      text alongside tool calls
        //   • `reasoning_format: "deepseek"` – extract <think> → reasoning_content
        //   • any other provider-specific key that sven doesn't model natively
        if let Some(map) = self.extra_body.as_object() {
            for (k, v) in map {
                body[k] = v.clone();
            }
        }

        debug!(
            driver = %self.driver_name,
            model = %self.model,
            tool_count = tools.len(),
            message_count = messages.len(),
            "sending completion request"
        );
        
        // Log full request body at trace level for debugging schema issues
        tracing::trace!(request_body = ?body, "full completion request");

        let mut http_req = self.client.post(&self.chat_url).json(&body);
        http_req = match self.auth_style {
            AuthStyle::Bearer => {
                let key = self.api_key.as_deref()
                    .context("API key not set; provide api_key or api_key_env in config")?;
                http_req.bearer_auth(key)
            }
            AuthStyle::ApiKeyHeader => {
                let key = self.api_key.as_deref()
                    .context("API key not set; provide api_key or api_key_env in config")?;
                http_req.header("api-key", key)
            }
            AuthStyle::None => http_req,
        };
        for (name, val) in &self.extra_headers {
            http_req = http_req.header(name.as_str(), val.as_str());
        }

        let resp = http_req.send().await
            .with_context(|| format!("{} request failed", self.driver_name))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("{} error {status}: {text}", self.driver_name);
        }

        let byte_stream = resp.bytes_stream();
        // SSE events can be split across multiple TCP packets.  Maintain a
        // line buffer across chunks; emit events only for complete lines.
        let event_stream = byte_stream
            .scan(String::new(), |buf, chunk| {
                let events: Vec<anyhow::Result<ResponseEvent>> = match chunk {
                    Ok(b) => {
                        buf.push_str(&String::from_utf8_lossy(&b));
                        drain_complete_sse_lines(buf)
                    }
                    Err(e) => vec![Err(anyhow::anyhow!(e))],
                };
                std::future::ready(Some(events))
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(event_stream))
    }
}

/// Parse a single complete SSE `data:` line into a [`ResponseEvent`].
///
/// Returns `None` for empty lines, comment lines, or unparseable data.
fn parse_sse_data_line(line: &str) -> Option<anyhow::Result<ResponseEvent>> {
    let data = line.strip_prefix("data: ")?.trim();
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return Some(Ok(ResponseEvent::Done));
    }
    let v: Value = serde_json::from_str(data).ok()?;
    Some(parse_sse_chunk(&v))
}

/// Drain all complete `\n`-terminated SSE lines from `buf`.
///
/// Any trailing incomplete line (bytes not yet terminated by `\n`) is left
/// in `buf` so it can be extended by the next TCP chunk.  This is necessary
/// because a single SSE event may be split across multiple TCP packets.
pub(crate) fn drain_complete_sse_lines(buf: &mut String) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events = Vec::new();
    while let Some(nl_pos) = buf.find('\n') {
        // Strip the optional Windows-style \r before \n.
        let line = buf[..nl_pos].trim_end_matches('\r').to_string();
        // Advance buffer past the consumed line including the \n.
        *buf = buf[nl_pos + 1..].to_string();
        if let Some(ev) = parse_sse_data_line(&line) {
            events.push(ev);
        }
    }
    events
}

fn role_str(r: &Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_sse_chunk(v: &Value) -> anyhow::Result<ResponseEvent> {
    // Usage-only chunk (emitted when stream_options.include_usage = true)
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        // OpenAI reports cached tokens in prompt_tokens_details.cached_tokens.
        // DeepSeek V3 reports them as prompt_cache_hit_tokens on the root
        // usage object.  We try OpenAI format first, then fall back to
        // DeepSeek's format so both providers are covered without extra
        // provider-specific dispatch.
        let cache_read_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|t| t.as_u64())
            .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(|t| t.as_u64()))
            .unwrap_or(0) as u32;
        return Ok(ResponseEvent::Usage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
            cache_read_tokens,
            cache_write_tokens: 0,
        });
    }

    // llama.cpp performance metrics (top-level `timings` object)
    // These arrive in the final SSE chunk with finish_reason=stop and provide
    // cache hit counts and generation speed that are incredibly useful for CI
    // debugging.  We convert them into a Usage event so the CI runner can emit
    // them as `[sven:tokens]` trace output.
    if let Some(timings) = v.get("timings") {
        let cache_n = timings["cache_n"].as_u64().unwrap_or(0) as u32;
        let prompt_n = timings["prompt_n"].as_u64().unwrap_or(0) as u32;
        let predicted_n = timings["predicted_n"].as_u64().unwrap_or(0) as u32;

        // llama.cpp reports cache hits + fresh tokens separately; combine them
        // into input_tokens for consistency with standard Usage reporting.
        return Ok(ResponseEvent::Usage {
            input_tokens: cache_n + prompt_n,
            output_tokens: predicted_n,
            cache_read_tokens: cache_n,
            cache_write_tokens: 0,
        });
    }

    let choice = &v["choices"][0];

    // finish_reason=length means the model hit its output-token limit.
    // Emit MaxTokens so the agent knows any pending tool-call arguments
    // are truncated.  The [DONE] sentinel that follows will emit Done.
    if choice["finish_reason"].as_str() == Some("length") {
        return Ok(ResponseEvent::MaxTokens);
    }

    let delta = &choice["delta"];

    // Tool call delta — OpenAI may send multiple parallel tool calls in one
    // chunk, each identified by an "index" field.  We only emit the first
    // element here because each SSE chunk carries exactly one tool-call delta
    // in practice; the index routes accumulation in the agent.
    if let Some(tool_calls) = delta.get("tool_calls") {
        if let Some(tc) = tool_calls.get(0) {
            let index = tc["index"].as_u64().unwrap_or(0) as u32;
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
            return Ok(ResponseEvent::ToolCall { index, id, name, arguments: args });
        }
    }

    // Thinking delta — two common field names for chain-of-thought reasoning:
    //   • `reasoning_content` — llama.cpp, Qwen3, DeepSeek-R1, xAI Grok-3-mini
    //   • `reasoning`         — OpenRouter (and some other aggregators)
    // Both carry the same semantics: readable CoT text that arrived before the
    // final answer.  Prefer `reasoning_content`; fall back to `reasoning`.
    let thinking_text = delta.get("reasoning_content").and_then(|c| c.as_str())
        .or_else(|| delta.get("reasoning").and_then(|c| c.as_str()));
    if let Some(thinking) = thinking_text {
        if !thinking.is_empty() {
            return Ok(ResponseEvent::ThinkingDelta(thinking.to_string()));
        }
    }

    // Text delta
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        return Ok(ResponseEvent::TextDelta(text.to_string()));
    }

    Ok(ResponseEvent::TextDelta(String::new()))
}

/// Convert a slice of [`Message`]s into the OpenAI wire-format JSON array.
///
/// Extracted as a free function so it can be unit-tested without making HTTP
/// requests.
///
/// **Parallel tool call coalescing**: OpenAI requires that all tool calls from
/// one assistant turn appear inside a *single* assistant message as a
/// `tool_calls` array.  Sven stores each tool call as a separate
/// `MessageContent::ToolCall` entry internally (easier to work with), so this
/// function merges consecutive `ToolCall` messages into one JSON object before
/// sending them to the API.
pub(crate) fn build_openai_messages(messages: &[crate::Message]) -> Vec<Value> {
    use crate::{ContentPart, MessageContent, ToolContentPart, ToolResultContent};

    fn tool_call_to_json(tool_call_id: &str, function: &crate::FunctionCall) -> Value {
        json!({
            "id": tool_call_id,
            "type": "function",
            "function": {
                "name": function.name,
                "arguments": function.arguments,
            }
        })
    }

    fn tool_result_to_json(tool_call_id: &str, content: &ToolResultContent) -> Value {
        let wire_content: Value = match content {
            ToolResultContent::Text(t) => json!(t),
            ToolResultContent::Parts(parts) if !parts.is_empty() => {
                let arr: Vec<Value> = parts.iter().map(|p| match p {
                    ToolContentPart::Text { text } => json!({ "type": "text", "text": text }),
                    ToolContentPart::Image { image_url } => json!({
                        "type": "image_url",
                        "image_url": { "url": image_url },
                    }),
                }).collect();
                json!(arr)
            }
            ToolResultContent::Parts(_) => json!(""),
        };
        json!({ "role": "tool", "tool_call_id": tool_call_id, "content": wire_content })
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len());
    let mut i = 0;

    while i < messages.len() {
        let m = &messages[i];

        // Merge consecutive ToolCall messages into one assistant message so
        // the wire format satisfies OpenAI's parallel-tool-call contract.
        if let MessageContent::ToolCall { tool_call_id, function } = &m.content {
            let mut calls = vec![tool_call_to_json(tool_call_id, function)];
            i += 1;
            while i < messages.len() {
                if let MessageContent::ToolCall { tool_call_id, function } = &messages[i].content {
                    calls.push(tool_call_to_json(tool_call_id, function));
                    i += 1;
                } else {
                    break;
                }
            }
            result.push(json!({ "role": "assistant", "tool_calls": calls }));
            continue;
        }

        let v = match &m.content {
            MessageContent::Text(t) => json!({
                "role": role_str(&m.role),
                "content": t,
            }),
            MessageContent::ContentParts(parts) if !parts.is_empty() => {
                let content: Vec<Value> = parts.iter().map(|p| match p {
                    ContentPart::Text { text } => json!({ "type": "text", "text": text }),
                    ContentPart::Image { image_url, detail } => {
                        let mut img_obj = json!({ "url": image_url });
                        if let Some(d) = detail {
                            img_obj["detail"] = json!(d);
                        }
                        json!({ "type": "image_url", "image_url": img_obj })
                    }
                }).collect();
                json!({ "role": role_str(&m.role), "content": content })
            }
            MessageContent::ContentParts(_) => {
                json!({ "role": role_str(&m.role), "content": "" })
            }
            MessageContent::ToolCall { .. } => unreachable!("handled above"),
            MessageContent::ToolResult { tool_call_id, content } => {
                tool_result_to_json(tool_call_id, content)
            }
        };
        result.push(v);
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProvider;

    fn make_provider() -> OpenAICompatProvider {
        OpenAICompatProvider::new(
            "test-compat",
            "test-model".into(),
            None,
            "http://localhost:9999/v1",
            Some(1024),
            Some(0.0),
            vec![],
            AuthStyle::None,
            serde_json::Value::Null,
        )
    }

    #[test]
    fn name_returns_driver_name() {
        let p = make_provider();
        assert_eq!(p.name(), "test-compat");
    }

    #[test]
    fn model_name_returns_model() {
        let p = make_provider();
        assert_eq!(p.model_name(), "test-model");
    }

    #[test]
    fn chat_url_appends_path() {
        let p = make_provider();
        assert_eq!(p.chat_url, "http://localhost:9999/v1/chat/completions");
    }

    #[test]
    fn base_url_trailing_slash_stripped() {
        let p = OpenAICompatProvider::new(
            "x", "m".into(), None,
            "http://localhost:1234/v1/",
            None, None, vec![], AuthStyle::None,
            serde_json::Value::Null,
        );
        assert_eq!(p.chat_url, "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn extra_headers_stored() {
        let p = OpenAICompatProvider::new(
            "openrouter", "m".into(), None,
            "https://openrouter.ai/api/v1", None, None,
            vec![("HTTP-Referer".into(), "https://example.com".into())],
            AuthStyle::Bearer,
            serde_json::Value::Null,
        );
        assert_eq!(p.extra_headers.len(), 1);
        assert_eq!(p.extra_headers[0].0, "HTTP-Referer");
    }

    // ── extra_body (driver_options) ───────────────────────────────────────────

    /// Verify that keys in extra_body are merged into the request JSON.
    #[test]
    fn extra_body_keys_are_merged_into_request() {
        use serde_json::json;

        let extra = json!({ "parse_tool_calls": false, "reasoning_format": "deepseek" });
        let p = OpenAICompatProvider::new(
            "llama", "qwen2.5".into(), None,
            "http://localhost:8080/v1", None, None,
            vec![], AuthStyle::None,
            extra,
        );

        // Simulate what complete() does: build a base body and merge extra_body.
        let mut body = json!({
            "model": p.model,
            "stream": true,
            "max_tokens": p.max_tokens,
        });
        if let Some(map) = p.extra_body.as_object() {
            for (k, v) in map {
                body[k] = v.clone();
            }
        }

        assert_eq!(body["parse_tool_calls"], json!(false));
        assert_eq!(body["reasoning_format"], json!("deepseek"));
        assert_eq!(body["model"], json!("qwen2.5"));
    }

    /// Verify that Null extra_body does not alter the request JSON.
    #[test]
    fn null_extra_body_does_not_alter_request() {
        use serde_json::json;

        let p = make_provider();
        let mut body = json!({ "model": p.model, "stream": true });
        if let Some(map) = p.extra_body.as_object() {
            for (k, v) in map {
                body[k] = v.clone();
            }
        }

        let obj = body.as_object().unwrap();
        assert_eq!(obj.len(), 2, "no extra keys should be inserted");
    }

    /// Verify that extra_body keys override sven-computed keys (user wins).
    #[test]
    fn extra_body_overrides_computed_keys() {
        use serde_json::json;

        let extra = json!({ "stream": false, "temperature": 0.9 });
        let p = OpenAICompatProvider::new(
            "test", "m".into(), None,
            "http://localhost/v1", None, Some(0.2), vec![], AuthStyle::None,
            extra,
        );

        let mut body = json!({ "stream": true, "temperature": p.temperature });
        if let Some(map) = p.extra_body.as_object() {
            for (k, v) in map {
                body[k] = v.clone();
            }
        }

        assert_eq!(body["stream"], json!(false), "extra_body should override stream");
        assert_eq!(body["temperature"], json!(0.9), "extra_body should override temperature");
    }

    // ── parse_sse_chunk ───────────────────────────────────────────────────────

    #[test]
    fn parse_sse_text_delta() {
        let v = serde_json::json!({
            "choices": [{ "delta": { "content": "hello" } }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "hello"));
    }

    #[test]
    fn parse_sse_empty_content_is_empty_text_delta() {
        let v = serde_json::json!({
            "choices": [{ "delta": { "content": "" } }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn parse_sse_no_content_no_tools_is_empty_text_delta() {
        let v = serde_json::json!({
            "choices": [{ "delta": {} }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.is_empty()));
    }

    #[test]
    fn parse_sse_tool_call_start_with_id_and_name() {
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": { "name": "shell", "arguments": "" }
                    }]
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { index, id, name, arguments }
                if *index == 0 && id == "call_abc" && name == "shell" && arguments.is_empty()),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_tool_call_nonzero_index() {
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 2,
                        "id": "call_xyz",
                        "function": { "name": "read_file", "arguments": "" }
                    }]
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { index, id, .. }
                if *index == 2 && id == "call_xyz"),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_tool_call_args_delta() {
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "",
                        "function": { "name": "", "arguments": "{\"cmd\": " }
                    }]
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { arguments, .. } if arguments == "{\"cmd\": "),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_usage_event() {
        let v = serde_json::json!({
            "usage": { "prompt_tokens": 100, "completion_tokens": 50 }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage { input_tokens: 100, output_tokens: 50, .. }),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_usage_event_with_cached_tokens() {
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 40,
                "prompt_tokens_details": { "cached_tokens": 150 }
            }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage {
                input_tokens: 200,
                output_tokens: 40,
                cache_read_tokens: 150,
                ..
            }),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_null_usage_falls_through_to_delta() {
        // When usage is null (not the final stats chunk), it should fall
        // through to delta parsing rather than emit a Usage event.
        let v = serde_json::json!({
            "usage": null,
            "choices": [{ "delta": { "content": "hi" } }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "hi"));
    }

    // ── Multimodal message serialization ────────────────────────────────────────

    #[test]
    fn plain_text_message_serialized_as_string_content() {
        use crate::Message;
        let msgs = vec![Message::user("hello world")];
        let json = build_openai_messages(&msgs);
        assert_eq!(json[0]["role"], "user");
        assert_eq!(json[0]["content"], "hello world");
    }

    #[test]
    fn content_parts_single_text_collapses_to_string() {
        // user_with_parts(single text) collapses to MessageContent::Text for
        // cleaner serialization — the wire format should be a plain string.
        use crate::{ContentPart, Message};
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "describe this".into() },
        ]);
        let json = build_openai_messages(&[msg]);
        assert_eq!(json[0]["content"], "describe this");
    }

    #[test]
    fn content_parts_with_image_serialized_as_image_url_block() {
        use crate::{ContentPart, Message};
        let data_url = "data:image/png;base64,iVBORw0KGgo=";
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "what is this?".into() },
            ContentPart::image(data_url),
        ]);
        let json = build_openai_messages(&[msg]);
        let content = &json[0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], data_url);
    }

    #[test]
    fn tool_result_parts_with_image_serialized_as_content_array() {
        use crate::{Message, ToolContentPart};
        let data_url = "data:image/jpeg;base64,/9j/4AAQ=";
        let msg = Message::tool_result_with_parts("tc-99", vec![
            ToolContentPart::Text { text: "image captured".into() },
            ToolContentPart::Image { image_url: data_url.into() },
        ]);
        let json = build_openai_messages(&[msg]);
        assert_eq!(json[0]["role"], "tool");
        assert_eq!(json[0]["tool_call_id"], "tc-99");
        let content = &json[0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], data_url);
    }

    #[test]
    fn tool_result_plain_text_serialized_as_string() {
        use crate::Message;
        let msg = Message::tool_result("tc-1", "just text");
        let json = build_openai_messages(&[msg]);
        assert_eq!(json[0]["content"], "just text");
    }

    #[test]
    fn image_with_detail_low_includes_detail_field() {
        use crate::{ContentPart, Message};
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "what logo is this?".into() },
            ContentPart::image_with_detail(url, "low"),
        ]);
        let json = build_openai_messages(&[msg]);
        let content = &json[0]["content"];
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], url);
        assert_eq!(content[1]["image_url"]["detail"], "low");
    }

    #[test]
    fn image_without_detail_omits_detail_field() {
        use crate::{ContentPart, Message};
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let msg = Message::user_with_parts(vec![
            ContentPart::Text { text: "describe".into() },
            ContentPart::image(url),
        ]);
        let json = build_openai_messages(&[msg]);
        let content = &json[0]["content"];
        assert_eq!(content[1]["type"], "image_url");
        // detail should be absent when None
        assert!(content[1]["image_url"]["detail"].is_null());
    }

    // ── SSE line-buffer regression tests ─────────────────────────────────────
    //
    // Root cause: the old `flat_map` processed each TCP byte chunk independently
    // with `str::lines()`.  When an SSE event was split across two TCP packets
    // the first half (no `\n`) was silently dropped because it couldn't be
    // parsed as complete JSON, and the second half was dropped because it had
    // no `data: ` prefix.  For parallel tool calls (many index values) this
    // caused:
    //   • `id` and `name` to be empty (those chunks were dropped)
    //   • argument fragments to fall into slot 0 via `unwrap_or(0)`
    //   • corrupted JSON argument strings in the session history
    //   • OpenAI 400 "empty string" error on the next round
    //
    // The fix: `drain_complete_sse_lines` maintains a persistent buffer across
    // chunks; only complete `\n`-terminated lines are parsed.

    #[test]
    fn drain_complete_lines_handles_single_complete_line() {
        let line = r#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        let mut buf = format!("data: {line}\n");
        let events = drain_complete_sse_lines(&mut buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(ResponseEvent::TextDelta(t)) if t == "hi"));
        assert!(buf.is_empty(), "buffer should be drained");
    }

    #[test]
    fn drain_complete_lines_retains_incomplete_last_line() {
        let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"hel";
        let mut buf = partial.to_string();
        let events = drain_complete_sse_lines(&mut buf);
        assert!(events.is_empty(), "no complete line yet");
        assert_eq!(buf, partial, "partial line must stay in buffer");
    }

    #[test]
    fn sse_event_split_across_two_chunks_is_parsed_correctly() {
        // Simulate an SSE event for a tool call where the JSON is delivered
        // in two TCP packets.  The old code would drop BOTH halves silently.
        let full_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":""}}]}}]}"#;
        let split = full_line.len() / 2;
        let chunk1 = &full_line[..split];
        let chunk2 = &full_line[split..];

        let mut buf = String::new();

        // First chunk: no newline yet — no events emitted
        buf.push_str(chunk1);
        let events1 = drain_complete_sse_lines(&mut buf);
        assert!(events1.is_empty(), "should not emit partial event");
        assert!(!buf.is_empty(), "buffer must hold partial line");

        // Second chunk + newline: completes the event
        buf.push_str(chunk2);
        buf.push('\n');
        let events2 = drain_complete_sse_lines(&mut buf);
        assert_eq!(events2.len(), 1, "should emit exactly one event");
        assert!(buf.is_empty());

        match &events2[0] {
            Ok(ResponseEvent::ToolCall { index, id, name, .. }) => {
                assert_eq!(*index, 0, "index should be 0");
                assert_eq!(id, "call_1", "id should be preserved");
                assert_eq!(name, "shell", "name should be preserved");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn multiple_sse_events_in_one_tcp_chunk_all_parsed() {
        // Two complete SSE events in a single TCP packet.
        let chunk = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c0\",\"function\":{\"name\":\"glob\",\"arguments\":\"\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c1\",\"function\":{\"name\":\"grep\",\"arguments\":\"\"}}]}}]}\n",
        );
        let mut buf = chunk.to_string();
        let events = drain_complete_sse_lines(&mut buf);
        assert_eq!(events.len(), 2, "both events should be parsed");
        assert!(buf.is_empty());

        match &events[0] {
            Ok(ResponseEvent::ToolCall { index, id, name, .. }) => {
                assert_eq!(*index, 0); assert_eq!(id, "c0"); assert_eq!(name, "glob");
            }
            other => panic!("unexpected first event: {other:?}"),
        }
        match &events[1] {
            Ok(ResponseEvent::ToolCall { index, id, name, .. }) => {
                assert_eq!(*index, 1); assert_eq!(id, "c1"); assert_eq!(name, "grep");
            }
            other => panic!("unexpected second event: {other:?}"),
        }
    }

    #[test]
    fn argument_chunk_split_does_not_corrupt_args() {
        // Simulate a tool call where the arguments are streamed in pieces and
        // the SSE line containing one argument fragment is split across two
        // TCP chunks.  The accumulated args_buf must contain only the correct
        // arguments for the right tool call.
        let args_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pattern\":"}}]}}]}"#;
        let split = 60; // split inside the JSON arguments string
        let chunk1 = &args_line[..split];
        let chunk2 = &args_line[split..];

        let mut buf = String::new();
        buf.push_str(chunk1);
        let e1 = drain_complete_sse_lines(&mut buf);
        assert!(e1.is_empty());

        buf.push_str(chunk2);
        buf.push('\n');
        let e2 = drain_complete_sse_lines(&mut buf);
        assert_eq!(e2.len(), 1);

        match &e2[0] {
            Ok(ResponseEvent::ToolCall { index, arguments, .. }) => {
                assert_eq!(*index, 0);
                assert_eq!(arguments, r#"{"pattern":"#, "args should be the complete fragment, not mixed");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn done_event_is_parsed_correctly() {
        let mut buf = "data: [DONE]\n".to_string();
        let events = drain_complete_sse_lines(&mut buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Ok(ResponseEvent::Done)));
    }

    #[test]
    fn windows_crlf_line_endings_are_handled() {
        let line = r#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        let mut buf = format!("data: {line}\r\n");
        let events = drain_complete_sse_lines(&mut buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(ResponseEvent::TextDelta(t)) if t == "hi"));
    }

    // ── Parallel tool call coalescing ────────────────────────────────────────

    #[test]
    fn two_consecutive_tool_call_messages_coalesced_into_one_assistant_message() {
        use crate::{FunctionCall, Message, MessageContent, Role};
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_1".into(),
                    function: FunctionCall { name: "glob".into(), arguments: r#"{"pattern":"*.c"}"#.into() },
                },
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_2".into(),
                    function: FunctionCall { name: "read_file".into(), arguments: r#"{"path":"main.c"}"#.into() },
                },
            },
            Message::tool_result("call_1", "found 3 files"),
            Message::tool_result("call_2", "int main() {}"),
        ];
        let json = build_openai_messages(&msgs);
        // Two tool calls → one assistant message + two tool messages = 3 total
        assert_eq!(json.len(), 3, "expected 3 wire messages, got {}", json.len());
        assert_eq!(json[0]["role"], "assistant");
        let calls = json[0]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[1]["id"], "call_2");
        assert_eq!(json[1]["role"], "tool");
        assert_eq!(json[1]["tool_call_id"], "call_1");
        assert_eq!(json[2]["role"], "tool");
        assert_eq!(json[2]["tool_call_id"], "call_2");
    }

    // ── DeepSeek cache hit token parsing ────────────────────────────────────

    #[test]
    fn parse_sse_deepseek_cache_hit_tokens_at_root() {
        // DeepSeek V3 puts cache metrics directly on the usage object, not
        // nested inside prompt_tokens_details.
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 500,
                "completion_tokens": 30,
                "prompt_cache_hit_tokens": 400,
                "prompt_cache_miss_tokens": 100,
            }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage {
                input_tokens: 500,
                output_tokens: 30,
                cache_read_tokens: 400,
                ..
            }),
            "unexpected event: {ev:?}"
        );
    }

    #[test]
    fn parse_sse_openai_format_takes_priority_over_deepseek_fallback() {
        // When both formats are present (hypothetical), the OpenAI nested
        // format takes priority (first branch of .or_else chain wins).
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 300,
                "completion_tokens": 20,
                "prompt_tokens_details": { "cached_tokens": 250 },
                "prompt_cache_hit_tokens": 999,
            }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(ev, ResponseEvent::Usage {
                cache_read_tokens: 250, // OpenAI nested value wins
                ..
            }),
            "unexpected event: {ev:?}"
        );
    }

    // ── Single tool call ─────────────────────────────────────────────────────

    #[test]
    fn single_tool_call_message_still_works() {
        use crate::{FunctionCall, Message, MessageContent, Role};
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_1".into(),
                    function: FunctionCall { name: "shell".into(), arguments: r#"{"command":"ls"}"#.into() },
                },
            },
            Message::tool_result("call_1", "file.txt"),
        ];
        let json = build_openai_messages(&msgs);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["role"], "assistant");
        let calls = json[0]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
    }

    // ── reasoning_content (llama.cpp / Qwen3 thinking) ───────────────────────

    #[test]
    fn reasoning_content_produces_thinking_delta() {
        // llama.cpp emits thinking via `reasoning_content` on the delta.
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "",
                    "reasoning_content": "Let me think about this..."
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ThinkingDelta(t) if t == "Let me think about this..."),
            "expected ThinkingDelta, got {ev:?}"
        );
    }

    #[test]
    fn reasoning_content_empty_string_falls_through_to_text_delta() {
        // When reasoning_content is present but empty, we should fall through
        // to the text content (e.g. the transition chunk when thinking ends).
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "The answer is 42.",
                    "reasoning_content": ""
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::TextDelta(t) if t == "The answer is 42."),
            "expected TextDelta after empty reasoning_content, got {ev:?}"
        );
    }

    #[test]
    fn reasoning_content_null_falls_through_to_text_delta() {
        // Some providers send `"reasoning_content": null` when not thinking.
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "hello",
                    "reasoning_content": null
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::TextDelta(t) if t == "hello"),
            "expected TextDelta when reasoning_content is null, got {ev:?}"
        );
    }

    #[test]
    fn reasoning_content_only_no_text_content() {
        // Pure thinking chunk — no content field at all.
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "Step 1: analyse the problem."
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ThinkingDelta(t) if t == "Step 1: analyse the problem."),
            "expected ThinkingDelta, got {ev:?}"
        );
    }

    #[test]
    fn reasoning_content_sse_round_trip_through_line_buffer() {
        // Verify that a split SSE line carrying reasoning_content is handled
        // the same way as a split text-delta line (the buffer reassembles it).
        let full_line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#;
        let split = full_line.len() / 2;
        let chunk1 = &full_line[..split];
        let chunk2 = &full_line[split..];

        let mut buf = String::new();
        buf.push_str(chunk1);
        let e1 = drain_complete_sse_lines(&mut buf);
        assert!(e1.is_empty(), "should not emit partial event");

        buf.push_str(chunk2);
        buf.push('\n');
        let e2 = drain_complete_sse_lines(&mut buf);
        assert_eq!(e2.len(), 1);
        assert!(
            matches!(&e2[0], Ok(ResponseEvent::ThinkingDelta(t)) if t == "thinking..."),
            "unexpected event: {:?}", e2[0]
        );
    }

    // ── llama.cpp timings ─────────────────────────────────────────────────────
    // llama.cpp emits performance metrics in a top-level `timings` object in
    // the final SSE chunk.  We parse this into a Usage event.

    #[test]
    fn llama_cpp_timings_produces_usage_event() {
        let v = serde_json::json!({
            "choices": [{"finish_reason": "stop", "index": 0, "delta": {}}],
            "timings": {
                "cache_n": 40,
                "prompt_n": 1,
                "prompt_ms": 109.438,
                "predicted_n": 60,
                "predicted_ms": 5783.6,
                "predicted_per_token_ms": 96.39,
                "predicted_per_second": 10.37
            }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::Usage {
                input_tokens: 41,      // cache_n + prompt_n
                output_tokens: 60,     // predicted_n
                cache_read_tokens: 40, // cache_n
                ..
            }),
            "expected Usage from llama.cpp timings, got {ev:?}"
        );
    }

    #[test]
    fn llama_cpp_timings_with_no_cache_hits() {
        let v = serde_json::json!({
            "choices": [{"finish_reason": "stop", "index": 0, "delta": {}}],
            "timings": {
                "cache_n": 0,
                "prompt_n": 50,
                "predicted_n": 30
            }
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::Usage {
                input_tokens: 50,
                output_tokens: 30,
                cache_read_tokens: 0,
                ..
            }),
            "expected Usage with no cache hits, got {ev:?}"
        );
    }

    // ── OpenRouter `reasoning` field ─────────────────────────────────────────
    // OpenRouter aggregator exposes reasoning via a `reasoning` field on the
    // delta (different name from llama.cpp's `reasoning_content`).

    #[test]
    fn openrouter_reasoning_field_produces_thinking_delta() {
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "",
                    "reasoning": "I should consider both sides."
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ThinkingDelta(t) if t == "I should consider both sides."),
            "expected ThinkingDelta from OpenRouter reasoning field, got {ev:?}"
        );
    }

    #[test]
    fn reasoning_content_takes_priority_over_reasoning() {
        // When both fields are present (hypothetical), `reasoning_content` wins.
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "preferred",
                    "reasoning": "fallback"
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ThinkingDelta(t) if t == "preferred"),
            "reasoning_content should take priority, got {ev:?}"
        );
    }

    #[test]
    fn openrouter_reasoning_empty_falls_through_to_text() {
        let v = serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "hello",
                    "reasoning": ""
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::TextDelta(t) if t == "hello"),
            "empty reasoning should fall through to text delta, got {ev:?}"
        );
    }
}
