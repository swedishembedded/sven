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
        let messages: Vec<Value> = build_openai_messages(&req.messages);

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
            "stream_options": { "include_usage": true },
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        debug!(driver = %self.driver_name, model = %self.model, "sending completion request");

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
                    Some(parse_sse_chunk(&v))
                })
                .collect();
            futures::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
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
        return Ok(ResponseEvent::Usage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
        });
    }

    let choice = &v["choices"][0];
    let delta = &choice["delta"];

    // Tool call delta
    if let Some(tool_calls) = delta.get("tool_calls") {
        if let Some(tc) = tool_calls.get(0) {
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
            return Ok(ResponseEvent::ToolCall { id, name, arguments: args });
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
pub(crate) fn build_openai_messages(messages: &[crate::Message]) -> Vec<Value> {
    use crate::{ContentPart, MessageContent, ToolContentPart, ToolResultContent};

    messages.iter().map(|m| {
        match &m.content {
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
                // Empty parts — serialize as empty text to avoid invalid API payloads.
                json!({ "role": role_str(&m.role), "content": "" })
            }
            MessageContent::ToolCall { tool_call_id, function } => json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": tool_call_id,
                    "type": "function",
                    "function": {
                        "name": function.name,
                        "arguments": function.arguments,
                    }
                }]
            }),
            MessageContent::ToolResult { tool_call_id, content } => {
                let wire_content: Value = match content {
                    ToolResultContent::Text(t) => json!(t),
                    ToolResultContent::Parts(parts) if !parts.is_empty() => {
                        let arr: Vec<Value> = parts.iter().map(|p| match p {
                            ToolContentPart::Text { text } => {
                                json!({ "type": "text", "text": text })
                            }
                            ToolContentPart::Image { image_url } => json!({
                                "type": "image_url",
                                "image_url": { "url": image_url },
                            }),
                        }).collect();
                        json!(arr)
                    }
                    ToolResultContent::Parts(_) => json!(""),
                };
                json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": wire_content,
                })
            }
        }
    }).collect()
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
        );
        assert_eq!(p.extra_headers.len(), 1);
        assert_eq!(p.extra_headers[0].0, "HTTP-Referer");
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
                        "id": "call_abc",
                        "function": { "name": "shell", "arguments": "" }
                    }]
                }
            }]
        });
        let ev = parse_sse_chunk(&v).unwrap();
        assert!(
            matches!(&ev, ResponseEvent::ToolCall { id, name, arguments }
                if id == "call_abc" && name == "shell" && arguments.is_empty()),
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
            matches!(ev, ResponseEvent::Usage { input_tokens: 100, output_tokens: 50 }),
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
}
