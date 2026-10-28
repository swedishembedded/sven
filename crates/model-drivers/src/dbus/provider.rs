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
    turns, CompletionRequest, ContentPart, Message, MessageContent, ResponseEvent, ResponseStream,
    Role, ToolChoice, ToolContentPart, ToolResultContent, Turn,
};

use super::blob;
use super::proxy::{self, BusKind, ManagerProxy};
use super::stream::{CancelOnDrop, Frame, FrameReader, Translator};
use crate::openai_compat::function_tool;

/// The state of one streamed completion.
struct StreamState {
    reader: FrameReader,
    translator: Translator,
    pending: std::collections::VecDeque<ResponseEvent>,
    want_logprobs: bool,
    guard: CancelOnDrop,
    finished: bool,
}

/// Configuration for [`DbusProvider`], read from `driver_options`.
#[derive(Debug, Clone)]
pub struct DbusOptions {
    pub bus: BusKind,
    pub service: String,
    pub object_path: String,
    /// Action name passed as `Subscribe`'s second argument.
    pub action: String,
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
            max_image_dim: 1024,
        }
    }
}

impl DbusOptions {
    /// Build options from a free-form `driver_options` JSON value. Missing
    /// keys keep their defaults; a key this driver does not know, or a value
    /// of the wrong type, is reported as a warning rather than ignored.
    pub fn from_driver_options(opts: &Value) -> Self {
        let mut out = Self::default();
        let Some(map) = opts.as_object() else {
            if !opts.is_null() {
                warn!(driver_options = %opts, "dbus driver_options is not a mapping; using defaults");
            }
            return out;
        };
        for (key, value) in map {
            let text = || value.as_str().map(str::to_owned);
            let applied = match key.as_str() {
                "bus" => text().map(|v| out.bus = BusKind::parse(&v)),
                "service" => text().map(|v| out.service = v),
                "object_path" => text().map(|v| out.object_path = v),
                "action" => text().map(|v| out.action = v),
                "max_image_dim" => value.as_u64().map(|v| out.max_image_dim = v as u32),
                "transport" => {
                    warn!("dbus driver_options.transport is no longer used: the stream carries blobs as file descriptors");
                    Some(())
                }
                _ => {
                    warn!(key = %key, "unknown dbus driver_options key ignored");
                    Some(())
                }
            };
            if applied.is_none() {
                warn!(key = %key, value = %value, "dbus driver_options value has the wrong type; default kept");
            }
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

    async fn manager(&self) -> Result<ManagerProxy<'static>> {
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
        // Only the estimated room left: brain bounds the reply by its context.
        let limit = if req.output_limit_is_room {
            None
        } else {
            req.max_output_tokens_override.or(self.max_tokens)
        };
        if let Some(max) = limit {
            params.insert("max_new".to_string(), json!(max));
        }
        // brain's generate action calls it `temp`. Without one brain samples
        // with what the checkpoint declares.
        if let Some(temperature) = crate::temperature(req, self.temperature) {
            params.insert("temp".to_string(), json!(temperature));
        }
        if let Some(p) = req.sampling.top_p {
            params.insert("top_p".to_string(), json!(p));
        }
        if let Some(k) = req.sampling.top_k {
            params.insert("top_k".to_string(), json!(k));
        }
        if let Some(seed) = req.sampling.seed {
            params.insert("seed".to_string(), json!(seed));
        }
        if req.want_logprobs {
            params.insert("logprobs".to_string(), json!(true));
            params.insert("return_token_ids".to_string(), json!(true));
        }
        // Like `messages`, brain declares `tools` and `tool_choice` as strings
        // holding JSON text.
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req.tools.iter().map(function_tool).collect();
            params.insert("tools".to_string(), json!(Value::Array(tools).to_string()));
            if req.tool_choice == ToolChoice::None {
                params.insert("tool_choice".to_string(), json!("\"none\""));
            }
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
    let mut out = Vec::with_capacity(messages.len());
    for turn in turns(messages) {
        match turn {
            Turn::Message(m) => {
                let mut message = json!({
                    "role": role_str(&m.role),
                    "content": content_text(&m.content),
                });
                with_reasoning(&mut message, m.reasoning.as_deref());
                out.push(message);
            }
            // One model turn, one message: its text and every call it made.
            Turn::Assistant {
                reasoning,
                content,
                calls,
            } => {
                let mut message = json!({
                    "role": "assistant",
                    "content": content.map(content_text).unwrap_or_default(),
                    "tool_calls": calls.iter().map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.function.name,
                            "arguments": c.function.arguments,
                        }
                    })).collect::<Vec<_>>(),
                });
                with_reasoning(&mut message, reasoning);
                out.push(message);
            }
            Turn::ToolResults(results) => {
                for m in results {
                    if let MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } = &m.content
                    {
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": flatten_tool_result(content),
                        }));
                    }
                }
            }
        }
    }
    out
}

/// Adds an assistant turn's reasoning as brain's `reasoning_content`; the
/// chat template renders it where the model was trained to see it.
fn with_reasoning(message: &mut Value, reasoning: Option<&str>) {
    if let Some(r) = reasoning.filter(|r| !r.is_empty()) {
        message["reasoning_content"] = json!(r);
    }
}

/// The text of a message that is not a tool call or result.
fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::ContentParts(parts) => flatten_parts(parts),
        MessageContent::ToolCall { .. } | MessageContent::ToolResult { .. } => String::new(),
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

// ─── ModelProvider ────────────────────────────────────────────────────────────

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
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
            blobs = in_fds.len(),
            "calling Brain1.Manager.Subscribe"
        );

        let proxy = self.manager().await?;
        let (job, fd) = proxy
            .subscribe(&self.model, &self.opts.action, &params, in_fds, &in_meta)
            .await
            .context("Brain1.Manager.Subscribe failed")?;
        let reader = FrameReader::new(std::os::fd::OwnedFd::from(fd))?;
        let state = StreamState {
            reader,
            translator: Translator::default(),
            pending: std::collections::VecDeque::new(),
            want_logprobs: req.want_logprobs,
            guard: CancelOnDrop {
                proxy: Some(proxy),
                job,
            },
            finished: false,
        };
        // Events as brain produces them; the stream ends after `Done` or an error.
        Ok(Box::pin(stream::unfold(state, |mut st| async move {
            loop {
                if let Some(event) = st.pending.pop_front() {
                    return Some((Ok(event), st));
                }
                if st.finished {
                    st.guard.proxy.take(); // completed: nothing to cancel
                    return None;
                }
                match st.reader.next().await {
                    Ok(Some(frame)) => {
                        let done = matches!(frame, Frame::Done { .. });
                        match st.translator.events(frame, st.want_logprobs) {
                            Ok(events) => st.pending.extend(events),
                            Err(e) => {
                                st.finished = true;
                                return Some((Err(e), st));
                            }
                        }
                        st.finished = done;
                    }
                    Ok(None) => {
                        st.finished = true;
                        return Some((
                            Err(anyhow::anyhow!(
                                "brain closed the stream before the reply was done"
                            )),
                            st,
                        ));
                    }
                    Err(e) => {
                        st.finished = true;
                        return Some((Err(e), st));
                    }
                }
            }
        })))
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
        assert_eq!(o.max_image_dim, 1024);
    }

    #[test]
    fn driver_options_are_read_from_json() {
        let o = DbusOptions::from_driver_options(&json!({
            "bus": "system",
            "service": "com.example.Other",
            "object_path": "/com/example/Other",
            "action": "chat",
            "max_image_dim": 512,
        }));
        assert_eq!(o.bus, BusKind::System);
        assert_eq!(o.service, "com.example.Other");
        assert_eq!(o.object_path, "/com/example/Other");
        assert_eq!(o.action, "chat");
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
                reasoning: None,
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

    /// A model turn is one assistant message: the text it said and the calls
    /// it made, as the model produced them. Sent apart, the chat template
    /// renders a turn that ends after "Let me run grep:" and a second turn
    /// holding only the call.
    #[test]
    fn a_turns_text_and_calls_are_one_assistant_message() {
        let call = |id: &str| Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        };
        let mut opening = Message::assistant("Let me run grep:");
        opening.reasoning = Some("grep finds it".into());
        let msgs = vec![
            opening,
            call("c1"),
            call("c2"),
            Message::tool_result("c1", "a"),
            Message::tool_result("c2", "b"),
        ];
        let out = flatten_messages(&msgs);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0]["content"], "Let me run grep:");
        assert_eq!(out[0]["reasoning_content"], "grep finds it");
        assert_eq!(out[0]["tool_calls"].as_array().unwrap().len(), 2);
        assert!(out[1].get("reasoning_content").is_none());
        assert_eq!(out[1]["tool_call_id"], "c1");
        assert_eq!(out[2]["tool_call_id"], "c2");
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
    fn params_carry_tools_in_the_openai_function_form() {
        let p = DbusProvider::new("m", DbusOptions::default(), None, None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![sven_model::ToolSchema {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                ..Default::default()
            }],
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        let tools: Value = serde_json::from_str(params["tools"].as_str().unwrap()).unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert!(
            params.get("tool_choice").is_none(),
            "auto is brain's default"
        );

        let none = CompletionRequest {
            tool_choice: ToolChoice::None,
            ..req
        };
        let params: Value = serde_json::from_str(&p.build_params(&none).unwrap()).unwrap();
        assert_eq!(params["tool_choice"], "\"none\"");
    }

    #[test]
    fn a_temperature_is_sent_only_when_set_and_under_brains_name() {
        let p = DbusProvider::new("m", DbusOptions::default(), None, None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert!(
            params.get("temp").is_none(),
            "unset, the checkpoint's declared sampling applies"
        );
        assert!(
            params.get("temperature").is_none(),
            "brain refuses an undeclared param"
        );

        let configured = DbusProvider::new("m", DbusOptions::default(), None, Some(0.6));
        let params: Value = serde_json::from_str(&configured.build_params(&req).unwrap()).unwrap();
        assert!((params["temp"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn params_ask_for_token_ids_with_logprobs() {
        let p = DbusProvider::new("m", DbusOptions::default(), None, None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            want_logprobs: true,
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert_eq!(params["logprobs"], true);
        assert_eq!(params["return_token_ids"], true);
    }

    #[test]
    fn params_omit_tools_when_there_are_none() {
        let p = DbusProvider::new("m", DbusOptions::default(), None, None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert!(params.get("tools").is_none());
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
    fn a_limit_that_is_only_the_room_left_is_not_sent() {
        let p = DbusProvider::new("brain/omni", DbusOptions::default(), Some(512), None);
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            max_output_tokens_override: Some(9000),
            output_limit_is_room: true,
            ..Default::default()
        };
        let params: Value = serde_json::from_str(&p.build_params(&req).unwrap()).unwrap();
        assert!(
            params.get("max_new").is_none(),
            "the server ends the reply at its context: {params}"
        );
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
