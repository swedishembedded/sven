// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`DbusProvider`]: brain's `generate` action presented as a chat
//! completion. See the [parent module](super) for configuration and scope.

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream;
use serde_json::{json, Value};
use tracing::{debug, warn};

use sven_model::{
    catalog::{InputModality, ModelCatalogEntry},
    CompletionRequest, ContentPart, Message, MessageContent, ResponseEvent, ResponseStream, Role,
    ToolContentPart, ToolResultContent,
};

use super::blob;
use super::proxy::{self, BusKind, ManagerProxy};

/// Configuration for [`DbusProvider`], read from `driver_options`.
#[derive(Debug, Clone)]
pub struct DbusOptions {
    pub bus: BusKind,
    pub service: String,
    pub object_path: String,
    /// Action name passed as `Run`'s second argument.
    pub action: String,
    /// Transport tag passed as `Run`'s last argument.
    pub transport: String,
    /// Longest image side sent as raw pixels; larger images are downscaled.
    pub max_image_dim: u32,
}

impl Default for DbusOptions {
    fn default() -> Self {
        Self {
            bus: BusKind::Session,
            service: proxy::DEFAULT_SERVICE.to_string(),
            object_path: proxy::DEFAULT_PATH.to_string(),
            action: "generate".to_string(),
            transport: "memfd".to_string(),
            max_image_dim: 1024,
        }
    }
}

impl DbusOptions {
    /// Build options from a free-form `driver_options` JSON value.
    ///
    /// Unknown keys are ignored; missing keys keep their defaults.
    pub fn from_driver_options(opts: &Value) -> Self {
        let mut out = Self::default();
        let Some(map) = opts.as_object() else {
            return out;
        };
        if let Some(v) = map.get("bus").and_then(|v| v.as_str()) {
            out.bus = BusKind::parse(v);
        }
        if let Some(v) = map.get("service").and_then(|v| v.as_str()) {
            out.service = v.to_string();
        }
        if let Some(v) = map.get("object_path").and_then(|v| v.as_str()) {
            out.object_path = v.to_string();
        }
        if let Some(v) = map.get("action").and_then(|v| v.as_str()) {
            out.action = v.to_string();
        }
        if let Some(v) = map.get("transport").and_then(|v| v.as_str()) {
            out.transport = v.to_string();
        }
        if let Some(v) = map.get("max_image_dim").and_then(|v| v.as_u64()) {
            out.max_image_dim = v as u32;
        }
        out
    }
}

/// A [`ModelProvider`](sven_model::ModelProvider) that speaks brain's D-Bus
/// interface.
pub struct DbusProvider {
    model: String,
    opts: DbusOptions,
    /// Output token budget forwarded as the `max_new` param.
    max_tokens: Option<u32>,
    /// Sampling temperature forwarded as the `temperature` param.
    temperature: Option<f32>,
    /// Established lazily on the first call: `from_config` is synchronous and
    /// must not fail merely because the server is not up yet.
    conn: tokio::sync::OnceCell<zbus::Connection>,
}

impl DbusProvider {
    pub fn new(
        model: impl Into<String>,
        opts: DbusOptions,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        Self {
            model: model.into(),
            opts,
            max_tokens,
            temperature,
            conn: tokio::sync::OnceCell::new(),
        }
    }

    /// Build a provider on an existing connection.
    ///
    /// Used by tests that serve a fake `Manager` over a peer-to-peer socket
    /// pair, where there is no bus to dial.
    pub fn with_connection(
        model: impl Into<String>,
        opts: DbusOptions,
        conn: zbus::Connection,
    ) -> Self {
        let cell = tokio::sync::OnceCell::new();
        let _ = cell.set(conn);
        Self {
            model: model.into(),
            opts,
            max_tokens: None,
            temperature: None,
            conn: cell,
        }
    }

    async fn connection(&self) -> Result<&zbus::Connection> {
        self.conn
            .get_or_try_init(|| async { self.opts.bus.connect().await })
            .await
    }

    async fn manager(&self) -> Result<ManagerProxy<'_>> {
        let conn = self.connection().await?;
        ManagerProxy::builder(conn)
            .destination(self.opts.service.clone())
            .context("invalid D-Bus service name")?
            .path(self.opts.object_path.clone())
            .context("invalid D-Bus object path")?
            .build()
            .await
            .context("building the Brain1.Manager proxy")
    }

    /// Assemble the `params` JSON object for one request.
    ///
    /// `messages` is a JSON **string** value inside the object, not a nested
    /// array — brain declares that parameter as `ParamType::Str`, so the array
    /// is serialised once into a string and the whole object once more.
    fn build_params(&self, req: &CompletionRequest) -> Result<String> {
        let flattened = flatten_messages(&req.messages);
        let messages_str =
            serde_json::to_string(&flattened).context("serialising flattened messages")?;

        let mut params = serde_json::Map::new();
        params.insert("messages".to_string(), json!(messages_str));
        if let Some(max) = req.max_output_tokens_override.or(self.max_tokens) {
            params.insert("max_new".to_string(), json!(max));
        }
        if let Some(t) = self.temperature {
            params.insert("temperature".to_string(), json!(t));
        }
        serde_json::to_string(&Value::Object(params)).context("serialising params object")
    }
}

// ─── Message flattening ───────────────────────────────────────────────────────

/// Flatten sven's message list into brain's array-of-objects contract.
///
/// The shape follows the OpenAI conventions already used by this codebase's
/// HTTP drivers (see `openai_compat::request::build_openai_messages`), minus
/// the multimodal content arrays: blobs travel out-of-band on file
/// descriptors, so image and audio parts collapse to the same `[image]` /
/// `[audio]` textual markers the Cohere driver and the compactor use.
pub fn flatten_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let role = role_str(&m.role);
            match &m.content {
                MessageContent::Text(t) => json!({ "role": role, "content": t }),
                MessageContent::ContentParts(parts) => {
                    json!({ "role": role, "content": flatten_parts(parts) })
                }
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } => json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": function.name,
                            "arguments": function.arguments,
                        }
                    }]
                }),
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } => json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": flatten_tool_result(content),
                }),
            }
        })
        .collect()
}

fn role_str(r: &Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn flatten_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.as_str(),
            ContentPart::Image { .. } => "[image]",
            ContentPart::Audio { .. } => "[audio]",
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn flatten_tool_result(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(t) => t.clone(),
        ToolResultContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                ToolContentPart::Text { text } => text.as_str(),
                ToolContentPart::Image { .. } => "[image]",
                ToolContentPart::Audio { .. } => "[audio]",
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

// ─── Blob extraction ──────────────────────────────────────────────────────────

/// The first image and first audio URL found scanning all messages in order.
///
/// At most one of each is sent, matching how the HTTP transports behave, so a
/// client that switches transports gets consistent results.  Extra blobs are
/// logged and dropped rather than raising an error, because failing a whole
/// turn over a second screenshot would be worse than ignoring it.
pub fn extract_blob_urls(messages: &[Message]) -> (Option<String>, Option<String>) {
    let mut image: Option<String> = None;
    let mut audio: Option<String> = None;
    let mut extra_images = 0usize;
    let mut extra_audio = 0usize;

    let mut visit = |url: &str, is_image: bool| {
        let slot = if is_image { &mut image } else { &mut audio };
        if slot.is_none() {
            *slot = Some(url.to_string());
        } else if is_image {
            extra_images += 1;
        } else {
            extra_audio += 1;
        }
    };

    for m in messages {
        match &m.content {
            MessageContent::ContentParts(parts) => {
                for p in parts {
                    match p {
                        ContentPart::Image { image_url, .. } => visit(image_url, true),
                        ContentPart::Audio { audio_url, .. } => visit(audio_url, false),
                        ContentPart::Text { .. } => {}
                    }
                }
            }
            MessageContent::ToolResult {
                content: ToolResultContent::Parts(parts),
                ..
            } => {
                for p in parts {
                    match p {
                        ToolContentPart::Image { image_url } => visit(image_url, true),
                        ToolContentPart::Audio { audio_url } => visit(audio_url, false),
                        ToolContentPart::Text { .. } => {}
                    }
                }
            }
            _ => {}
        }
    }

    if extra_images > 0 || extra_audio > 0 {
        warn!(
            extra_images,
            extra_audio,
            "the D-Bus transport sends at most one image and one audio blob per \
             request; extra attachments were dropped"
        );
    }
    (image, audio)
}

// ─── Reply decoding ───────────────────────────────────────────────────────────

/// Pull the generated text out of a `Run` reply.
///
/// `out_fds["text"]` is preferred; the `result` JSON's `"text"` field is the
/// fallback for servers that inline short replies.
fn reply_text(
    result_json: &str,
    out_fds: &HashMap<String, zbus::zvariant::OwnedFd>,
) -> Result<String> {
    if let Some(fd) = out_fds.get("text") {
        return blob::read_fd_to_string(fd);
    }
    let parsed: Value = serde_json::from_str(result_json)
        .with_context(|| format!("parsing Run result JSON: {result_json:.256}"))?;
    parsed
        .get("text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Run reply carried neither an out_fds[\"text\"] blob nor a \
                 `text` field in its result JSON: {result_json:.256}"
            )
        })
}

/// Best-effort token counts from the `result` JSON.
///
/// Recognises both a nested `usage` object and flat `num_tokens`-style keys;
/// anything unrecognised simply reports zero rather than failing the turn.
fn reply_usage(result_json: &str) -> (u32, u32) {
    let Ok(v) = serde_json::from_str::<Value>(result_json) else {
        return (0, 0);
    };
    let pick = |obj: &Value, keys: &[&str]| -> u32 {
        for k in keys {
            if let Some(n) = obj.get(*k).and_then(|x| x.as_u64()) {
                return n as u32;
            }
        }
        0
    };
    let usage = v.get("usage").unwrap_or(&v);
    let input = pick(
        usage,
        &["input_tokens", "prompt_tokens", "num_input_tokens"],
    );
    let output = pick(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "num_tokens",
            "num_output_tokens",
        ],
    );
    (input, output)
}

// ─── ModelProvider ────────────────────────────────────────────────────────────

#[async_trait]
impl sven_model::ModelProvider for DbusProvider {
    fn name(&self) -> &str {
        "dbus"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    /// This transport exists precisely to carry images and audio, so it always
    /// advertises all three modalities regardless of the static catalog (which
    /// cannot know about a self-hosted model).
    fn input_modalities(&self) -> Vec<InputModality> {
        vec![
            InputModality::Text,
            InputModality::Image,
            InputModality::Audio,
        ]
    }

    async fn list_models(&self) -> Result<Vec<ModelCatalogEntry>> {
        let entry_for = |id: &str| ModelCatalogEntry {
            id: id.to_string(),
            name: id.to_string(),
            provider: "dbus".to_string(),
            context_window: self.max_tokens.unwrap_or(32_768),
            max_output_tokens: self.max_tokens.unwrap_or(4_096),
            description: "brain model served over D-Bus".to_string(),
            input_modalities: vec![
                InputModality::Text,
                InputModality::Image,
                InputModality::Audio,
            ],
        };

        let proxy = match self.manager().await {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "D-Bus list_models: could not reach the server");
                return Ok(vec![entry_for(&self.model)]);
            }
        };
        match proxy.list_models().await {
            Ok(ids) => Ok(ids.iter().map(|id| entry_for(id)).collect()),
            Err(e) => {
                // Informational call only — never fail the caller over it.
                debug!(error = %e, "D-Bus ListModels failed; reporting the configured model only");
                Ok(vec![entry_for(&self.model)])
            }
        }
    }

    async fn complete(&self, req: CompletionRequest) -> Result<ResponseStream> {
        let params = self.build_params(&req)?;

        // Encode at most one image and one audio blob into sealed memfds.
        let (image_url, audio_url) = extract_blob_urls(&req.messages);
        let mut in_fds: HashMap<String, zbus::zvariant::OwnedFd> = HashMap::new();
        let mut meta = serde_json::Map::new();

        if let Some(url) = &image_url {
            let encoded = blob::encode_image(url, self.opts.max_image_dim)
                .context("encoding the image blob for D-Bus")?;
            in_fds.insert(
                "image".to_string(),
                blob::memfd_with_bytes("image", &encoded.bytes)?,
            );
            meta.insert("image".to_string(), encoded.meta);
        }
        if let Some(url) = &audio_url {
            let encoded = blob::encode_audio(url).context("encoding the audio blob for D-Bus")?;
            in_fds.insert(
                "audio".to_string(),
                blob::memfd_with_bytes("audio", &encoded.bytes)?,
            );
            meta.insert("audio".to_string(), encoded.meta);
        }
        let in_meta = serde_json::to_string(&Value::Object(meta)).context("serialising in_meta")?;

        debug!(
            model = %self.model,
            action = %self.opts.action,
            transport = %self.opts.transport,
            blobs = in_fds.len(),
            "calling Brain1.Manager.Run"
        );

        let proxy = self.manager().await?;
        let (result_json, out_fds, _out_meta) = proxy
            .run(
                &self.model,
                &self.opts.action,
                &params,
                in_fds,
                &in_meta,
                &self.opts.transport,
            )
            .await
            .context("Brain1.Manager.Run failed")?;

        let text = reply_text(&result_json, &out_fds)?;
        let (input_tokens, output_tokens) = reply_usage(&result_json);

        // One-shot: the whole reply arrives at once (see the module docs).
        let events: Vec<Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta(text)),
            Ok(ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::FunctionCall;

    #[test]
    fn bus_kind_parses_known_names() {
        assert_eq!(BusKind::parse("session"), BusKind::Session);
        assert_eq!(BusKind::parse("system"), BusKind::System);
        assert_eq!(BusKind::parse(""), BusKind::Session);
    }

    #[test]
    fn bus_kind_treats_anything_else_as_an_address() {
        assert_eq!(
            BusKind::parse("unix:path=/run/brain/bus"),
            BusKind::Address("unix:path=/run/brain/bus".into())
        );
    }

    #[test]
    fn driver_options_defaults_match_the_documented_values() {
        let o = DbusOptions::from_driver_options(&Value::Null);
        assert_eq!(o.bus, BusKind::Session);
        assert_eq!(o.service, "com.swedishembedded.Brain1");
        assert_eq!(o.object_path, "/com/swedishembedded/Brain1");
        assert_eq!(o.action, "generate");
        assert_eq!(o.transport, "memfd");
        assert_eq!(o.max_image_dim, 1024);
    }

    #[test]
    fn driver_options_are_read_from_json() {
        let o = DbusOptions::from_driver_options(&json!({
            "bus": "system",
            "service": "com.example.Other",
            "object_path": "/com/example/Other",
            "action": "chat",
            "transport": "shm",
            "max_image_dim": 512,
        }));
        assert_eq!(o.bus, BusKind::System);
        assert_eq!(o.service, "com.example.Other");
        assert_eq!(o.object_path, "/com/example/Other");
        assert_eq!(o.action, "chat");
        assert_eq!(o.transport, "shm");
        assert_eq!(o.max_image_dim, 512);
    }

    // ── Message flattening ────────────────────────────────────────────────────

    #[test]
    fn flattens_plain_messages_with_roles() {
        let msgs = vec![
            Message::system("be terse"),
            Message::user("hi"),
            Message::assistant("hello"),
        ];
        let out = flatten_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "be terse");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[2]["role"], "assistant");
    }

    #[test]
    fn flattens_multimodal_parts_to_text_with_markers() {
        let msg = Message::user_with_parts(vec![
            ContentPart::text("what is this?"),
            ContentPart::image("data:image/png;base64,AAA"),
            ContentPart::audio("data:audio/wav;base64,BBB"),
        ]);
        let out = flatten_messages(&[msg]);
        let content = out[0]["content"].as_str().unwrap();
        assert!(content.contains("what is this?"), "{content}");
        assert!(content.contains("[image]"), "{content}");
        assert!(content.contains("[audio]"), "{content}");
        assert!(!content.contains("base64"), "blobs must not be inlined");
    }

    #[test]
    fn flattens_tool_calls_and_results() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c1".into(),
                    function: FunctionCall {
                        name: "grep".into(),
                        arguments: "{}".into(),
                    },
                },
            },
            Message::tool_result("c1", "no matches"),
        ];
        let out = flatten_messages(&msgs);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "grep");
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "c1");
        assert_eq!(out[1]["content"], "no matches");
    }

    #[test]
    fn flattens_tool_result_parts() {
        let msg = Message::tool_result_with_parts(
            "c1",
            vec![
                ToolContentPart::Text {
                    text: "chart:".into(),
                },
                ToolContentPart::Image {
                    image_url: "data:image/png;base64,AAA".into(),
                },
            ],
        );
        let out = flatten_messages(&[msg]);
        assert_eq!(out[0]["content"], "chart:\n[image]");
    }

    // ── Params ────────────────────────────────────────────────────────────────

    #[test]
    fn params_carry_messages_as_a_json_string() {
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), Some(512), None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            ..Default::default()
        };
        let params_str = p.build_params(&req).unwrap();
        let params: Value = serde_json::from_str(&params_str).unwrap();

        // Double-encoded on purpose: brain declares `messages` as ParamType::Str.
        let inner = params["messages"]
            .as_str()
            .expect("messages must be a JSON *string* value, not a nested array");
        let arr: Value = serde_json::from_str(inner).unwrap();
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hi");
        assert_eq!(params["max_new"], 512);
    }

    #[test]
    fn params_omit_max_new_when_unconfigured() {
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), None, None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert!(params.get("max_new").is_none());
    }

    #[test]
    fn per_request_override_wins_over_configured_max_tokens() {
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), Some(512), None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            max_output_tokens_override: Some(16),
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert_eq!(params["max_new"], 16);
    }

    // ── Blob extraction ───────────────────────────────────────────────────────

    #[test]
    fn extracts_the_first_image_and_audio_in_order() {
        let msgs = vec![
            Message::user_with_parts(vec![
                ContentPart::image("img-1"),
                ContentPart::audio("aud-1"),
            ]),
            Message::user_with_parts(vec![
                ContentPart::image("img-2"),
                ContentPart::audio("aud-2"),
            ]),
        ];
        let (img, aud) = extract_blob_urls(&msgs);
        assert_eq!(img.as_deref(), Some("img-1"));
        assert_eq!(aud.as_deref(), Some("aud-1"));
    }

    #[test]
    fn extracts_none_from_plain_text_messages() {
        let (img, aud) = extract_blob_urls(&[Message::user("hi")]);
        assert!(img.is_none());
        assert!(aud.is_none());
    }

    #[test]
    fn extracts_blobs_from_tool_results_too() {
        let msg = Message::tool_result_with_parts(
            "c1",
            vec![ToolContentPart::Image {
                image_url: "tool-img".into(),
            }],
        );
        let (img, _) = extract_blob_urls(&[msg]);
        assert_eq!(img.as_deref(), Some("tool-img"));
    }

    // ── Reply decoding ────────────────────────────────────────────────────────

    #[test]
    fn reply_text_falls_back_to_the_result_json_field() {
        let out = reply_text(r#"{"text": "hello"}"#, &HashMap::new()).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn reply_text_prefers_the_out_fd() {
        let mut fds = HashMap::new();
        fds.insert(
            "text".to_string(),
            blob::memfd_with_bytes("text", b"from fd").unwrap(),
        );
        let out = reply_text(r#"{"text": "from json"}"#, &fds).unwrap();
        assert_eq!(out, "from fd");
    }

    #[test]
    fn reply_text_errors_when_there_is_no_text_anywhere() {
        let err = reply_text(r#"{"status": "ok"}"#, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("neither"), "{err}");
    }

    #[test]
    fn reply_usage_reads_a_nested_usage_object() {
        let (i, o) = reply_usage(r#"{"usage": {"input_tokens": 7, "output_tokens": 3}}"#);
        assert_eq!((i, o), (7, 3));
    }

    #[test]
    fn reply_usage_reads_flat_num_tokens() {
        let (_, o) = reply_usage(r#"{"text": "hi", "num_tokens": 5}"#);
        assert_eq!(o, 5);
    }

    #[test]
    fn reply_usage_is_zero_for_unparsable_json() {
        assert_eq!(reply_usage("not json"), (0, 0));
    }

    // ── Trait surface ─────────────────────────────────────────────────────────

    #[test]
    fn advertises_all_three_modalities() {
        use sven_model::ModelProvider as _;
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), None, None);
        let m = p.input_modalities();
        assert!(m.contains(&InputModality::Text));
        assert!(m.contains(&InputModality::Image));
        assert!(m.contains(&InputModality::Audio));
        assert!(p.supports_images());
        assert!(p.supports_audio());
    }

    #[test]
    fn reports_its_name_and_model() {
        use sven_model::ModelProvider as _;
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), None, None);
        assert_eq!(p.name(), "dbus");
        assert_eq!(p.model_name(), "brain/omni");
    }
}
