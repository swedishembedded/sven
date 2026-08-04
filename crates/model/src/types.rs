// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};

// ─── Content part types ───────────────────────────────────────────────────────

/// A single content part in a multi-part message.
///
/// Used for user and assistant messages that mix text with images or audio.
/// Images and audio are always represented as data URLs
/// (`data:<mime>;base64,<b64>`) or HTTPS URLs for providers that accept
/// remote references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Audio {
        /// Data URL: `data:audio/wav;base64,<b64>`. WAV only for now.
        audio_url: String,
        /// Wire format hint ("wav"|"mp3"), OpenAI's `input_audio.format` field.
        /// Derived from the data URL's MIME subtype when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    Image {
        /// Data URL (`data:image/png;base64,...`) or HTTPS URL.
        image_url: String,
        /// OpenAI vision detail level: `"low"`, `"high"`, or `"auto"`.
        ///
        /// - `"low"` → always 85 tokens regardless of image size; good for logos
        ///   and small thumbnails where fine detail is not required.
        /// - `"high"` → tile-based token counting; better recognition quality.
        /// - `"auto"` (default when `None`) → the provider chooses.
        ///
        /// Ignored by Anthropic, Google, Bedrock, and Cohere (OpenAI-only concept).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl ContentPart {
    /// Convenience constructor for a plain text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Convenience constructor for an image part with the provider default detail.
    pub fn image(image_url: impl Into<String>) -> Self {
        Self::Image {
            image_url: image_url.into(),
            detail: None,
        }
    }

    /// Convenience constructor for an image with an explicit OpenAI detail level.
    ///
    /// `detail` should be `"low"`, `"high"`, or `"auto"`.
    pub fn image_with_detail(image_url: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Image {
            image_url: image_url.into(),
            detail: Some(detail.into()),
        }
    }

    /// Convenience constructor for an audio part; the wire format is derived
    /// from the data URL's MIME subtype (see [`ContentPart::audio_format`]).
    pub fn audio(audio_url: impl Into<String>) -> Self {
        Self::Audio {
            audio_url: audio_url.into(),
            format: None,
        }
    }

    /// Convenience constructor for an audio part with an explicit wire format.
    ///
    /// `format` should be `"wav"` or `"mp3"` (OpenAI's `input_audio.format`).
    pub fn audio_with_format(audio_url: impl Into<String>, format: impl Into<String>) -> Self {
        Self::Audio {
            audio_url: audio_url.into(),
            format: Some(format.into()),
        }
    }

    /// Wire format string for an audio part.
    ///
    /// Returns the explicit `format` when set, otherwise derives it from the
    /// data URL's MIME subtype (`audio/wav` → `wav`, `audio/mpeg` → `mp3`).
    /// Falls back to `"wav"` for unrecognised audio URLs, and returns `""` for
    /// non-audio parts.
    pub fn audio_format(&self) -> &str {
        let Self::Audio { audio_url, format } = self else {
            return "";
        };
        if let Some(f) = format {
            return f.as_str();
        }
        mime_to_audio_format(audio_url)
    }
}

/// Derive an OpenAI `input_audio.format` value from a data URL (or plain URL).
///
/// Recognises the MIME subtypes brain and the OpenAI API care about; anything
/// unknown falls back to `"wav"`, which is the only format brain decodes.
fn mime_to_audio_format(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    let mime = lower
        .strip_prefix("data:")
        .and_then(|rest| rest.split(&[';', ','][..]).next())
        .unwrap_or("");
    match mime {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        _ => {
            // Not a data URL (or an unknown subtype): fall back to the file
            // extension when one is present, else assume WAV.
            if lower.ends_with(".mp3") {
                "mp3"
            } else {
                "wav"
            }
        }
    }
}

/// Content returned by a tool - either a plain string or structured parts.
///
/// The `Parts` variant allows a tool to return text and image blocks together.
/// Providers serialize this into their API-specific wire format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Parts(Vec<ToolContentPart>),
}

impl ToolResultContent {
    /// Lossy conversion to plain text (images are omitted).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            Self::Parts(_) => None,
        }
    }

    /// Collect all image URLs embedded in this content.
    pub fn image_urls(&self) -> Vec<&str> {
        match self {
            Self::Text(_) => vec![],
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ToolContentPart::Image { image_url } => Some(image_url.as_str()),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Collect all audio URLs embedded in this content.
    pub fn audio_urls(&self) -> Vec<&str> {
        match self {
            Self::Text(_) => vec![],
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ToolContentPart::Audio { audio_url } => Some(audio_url.as_str()),
                    _ => None,
                })
                .collect(),
        }
    }
}

impl From<String> for ToolResultContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for ToolResultContent {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

impl std::fmt::Display for ToolResultContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => write!(f, "{t}"),
            Self::Parts(parts) => {
                let text = parts
                    .iter()
                    .filter_map(|p| match p {
                        ToolContentPart::Text { text } => Some(text.as_str()),
                        ToolContentPart::Image { .. } | ToolContentPart::Audio { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                write!(f, "{text}")
            }
        }
    }
}

/// A single content part in a tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContentPart {
    Text {
        text: String,
    },
    Image {
        /// Data URL (`data:image/png;base64,...`).
        image_url: String,
    },
    Audio {
        /// Data URL (`data:audio/wav;base64,...`).
        audio_url: String,
    },
}

// ─── Data URL helpers ─────────────────────────────────────────────────────────

/// Parse a data URL of the form `data:<mime>;base64,<b64>` and return
/// `Ok((mime_type, base64_string))`.  Returns `Err` for non-data-URLs so
/// callers can fall back to treating the string as a plain HTTPS URL.
/// Approximate the token cost of an audio part from its data URL.
///
/// Decoding the audio just to count tokens would be wasteful, so this
/// estimates from the base64 payload size: assume 16-bit 16 kHz mono PCM
/// (32 000 bytes/s) and roughly 50 tokens per second of audio, with a
/// 100-token floor so short clips are never counted as free.
fn audio_approx_tokens(audio_url: &str) -> usize {
    let b64_len = audio_url.split_once(',').map(|(_, d)| d.len()).unwrap_or(0);
    let bytes = b64_len / 4 * 3;
    let seconds = bytes as f64 / 32_000.0;
    ((seconds * 50.0).round() as usize).max(100)
}

pub fn parse_data_url_parts(url: &str) -> Result<(String, String), &'static str> {
    let rest = url.strip_prefix("data:").ok_or("not a data URL")?;
    let (meta, b64) = rest.split_once(',').ok_or("malformed data URL")?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta).to_string();
    Ok((mime, b64.to_string()))
}

// ─── Message types ────────────────────────────────────────────────────────────

/// A single message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: id.into(),
                content: ToolResultContent::Text(content.into()),
            },
        }
    }

    /// Construct a tool result that contains text plus one or more image parts.
    ///
    /// If `parts` is empty, this falls back to `ToolResultContent::Text("")` to
    /// avoid sending an empty content array to provider APIs.
    pub fn tool_result_with_parts(id: impl Into<String>, parts: Vec<ToolContentPart>) -> Self {
        let content = if parts.is_empty() {
            ToolResultContent::Text(String::new())
        } else if parts.len() == 1 {
            // Collapse single text part for cleaner serialization
            if let ToolContentPart::Text { text } = &parts[0] {
                ToolResultContent::Text(text.clone())
            } else {
                ToolResultContent::Parts(parts)
            }
        } else {
            ToolResultContent::Parts(parts)
        };
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: id.into(),
                content,
            },
        }
    }

    /// Construct a user message from a list of content parts (text + images).
    ///
    /// If `parts` is empty, falls back to `MessageContent::Text("")`.
    /// If `parts` contains a single text item, collapses to `MessageContent::Text`.
    pub fn user_with_parts(parts: Vec<ContentPart>) -> Self {
        let content = if parts.is_empty() {
            MessageContent::Text(String::new())
        } else if parts.len() == 1 {
            if let ContentPart::Text { text } = &parts[0] {
                MessageContent::Text(text.clone())
            } else {
                MessageContent::ContentParts(parts)
            }
        } else {
            MessageContent::ContentParts(parts)
        };
        Self {
            role: Role::User,
            content,
        }
    }

    /// Return the plain text of this message, if it has exactly one text part.
    pub fn as_text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(t) => Some(t),
            MessageContent::ContentParts(parts) if parts.len() == 1 => match &parts[0] {
                ContentPart::Text { text } => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    /// Collect all audio URLs present in this message (user or tool content).
    pub fn audio_urls(&self) -> Vec<&str> {
        match &self.content {
            MessageContent::ContentParts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Audio { audio_url, .. } => Some(audio_url.as_str()),
                    _ => None,
                })
                .collect(),
            MessageContent::ToolResult { content, .. } => content.audio_urls(),
            _ => vec![],
        }
    }

    /// Collect all image URLs present in this message (user or tool content).
    pub fn image_urls(&self) -> Vec<&str> {
        match &self.content {
            MessageContent::ContentParts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Image { image_url, .. } => Some(image_url.as_str()),
                    _ => None,
                })
                .collect(),
            MessageContent::ToolResult { content, .. } => content.image_urls(),
            _ => vec![],
        }
    }

    /// Approximate token count used for context management.
    ///
    /// Uses a 4-chars-per-token heuristic for text.  Images use OpenAI's token
    /// estimates: 85 tokens for `detail = "low"`, 765 tokens otherwise
    /// (the typical auto/high estimate for a 512×512 region).
    pub fn approx_tokens(&self) -> usize {
        let chars = match &self.content {
            MessageContent::Text(t) => t.len(),
            MessageContent::ContentParts(parts) => parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.len(),
                    ContentPart::Audio { audio_url, .. } => audio_approx_tokens(audio_url) * 4,
                    ContentPart::Image { detail, .. } => {
                        // "low" → fixed 85 tokens regardless of image size.
                        // auto / high / None → ~765 tokens (conservative upper bound).
                        let tokens = if detail.as_deref() == Some("low") {
                            85
                        } else {
                            765
                        };
                        tokens * 4
                    }
                })
                .sum(),
            MessageContent::ToolCall { function, .. } => {
                function.name.len() + function.arguments.len()
            }
            MessageContent::ToolResult { content, .. } => match content {
                ToolResultContent::Text(t) => t.len(),
                ToolResultContent::Parts(parts) => parts
                    .iter()
                    .map(|p| match p {
                        ToolContentPart::Text { text } => text.len(),
                        ToolContentPart::Image { .. } => 765 * 4,
                        ToolContentPart::Audio { audio_url } => audio_approx_tokens(audio_url) * 4,
                    })
                    .sum(),
            },
        };
        (chars / 4).max(1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// The content of a message.
///
/// - `Text` - simple string (most messages)
/// - `ContentParts` - mixed text + image parts for multimodal user turns
/// - `ToolCall` - the assistant requests a tool invocation
/// - `ToolResult` - the result of a tool call, optionally with image parts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    ContentParts(Vec<ContentPart>),
    ToolCall {
        tool_call_id: String,
        function: FunctionCall,
    },
    ToolResult {
        tool_call_id: String,
        content: ToolResultContent,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded argument object
    pub arguments: String,
}

/// How the model should constrain its response format.
///
/// Used by drivers that support structured output (OpenAI / OpenRouter
/// `response_format`).  Providers that do not support it ignore the field; the
/// caller is expected to *also* describe the schema in the prompt and always
/// post-parse the result, so structured output works uniformly across drivers.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFormat {
    /// Constrain output to a syntactically valid JSON object (no schema).
    JsonObject,
    /// Constrain output to a specific JSON schema (OpenAI strict mode).
    JsonSchema {
        /// A short name identifying the schema (required by OpenAI).
        name: String,
        /// The JSON Schema the response must conform to.
        schema: serde_json::Value,
    },
}

/// A tool schema provided to the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema of the parameters object
    pub parameters: serde_json::Value,
    /// Whether this tool came from an external MCP server.
    ///
    /// When `true`, the Anthropic provider places a cache breakpoint after the
    /// last core tool AND after the last MCP tool.  This preserves the core
    /// tools cache (BP1) when MCP servers change, while only invalidating the
    /// cheaper MCP section (BP2).
    ///
    /// Not serialized in the API request; stripped before sending.
    #[serde(skip, default)]
    pub is_mcp: bool,
}

/// Request sent to a model provider.
#[derive(Debug, Clone, Default)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub stream: bool,
    /// Dynamic context (e.g. git branch/commit, CI info) that should NOT be
    /// included in the cached portion of the system prompt.
    ///
    /// When `None`, all context is already in `messages[0]` (system message)
    /// as usual.  When `Some`, the Anthropic provider appends this as a second
    /// system block *without* `cache_control`, so only the stable prefix is
    /// cached.  Other providers append it to the system message text.
    pub system_dynamic_suffix: Option<String>,
    /// Stable session identifier forwarded to providers that support an
    /// explicit cache key (e.g. OpenRouter's `prompt_cache_key`).  Using
    /// the same key across all requests in a session encourages the provider
    /// to reuse its prefix KV-cache for identical prefixes even when
    /// automatic prefix caching would not otherwise trigger.
    ///
    /// Providers that do not use this field (OpenAI, Anthropic, Google, ...)
    /// silently ignore it.
    pub cache_key: Option<String>,
    /// Optional per-request override for max output tokens (e.g. for short
    /// title generation). When set, providers use this instead of their
    /// configured default for this request only.
    pub max_output_tokens_override: Option<u32>,
    /// Number of "core" (non-MCP) tools at the start of `tools`.
    ///
    /// When > 0 and `tools.len() > core_tool_count`, the Anthropic provider
    /// places a cache breakpoint after `tools[core_tool_count - 1]` (BP1) and
    /// another after `tools.last()` (BP2).  When 0, only one breakpoint is
    /// placed at the end of the entire tools list (existing behavior).
    pub core_tool_count: usize,
    /// Optional structured-output constraint.
    ///
    /// When set, drivers that support it (OpenAI / OpenRouter) add a
    /// `response_format` field to the request body so the model returns valid
    /// JSON (optionally matching a schema).  Drivers that do not support it
    /// ignore the field; callers must always post-parse to stay portable.
    pub response_format: Option<ResponseFormat>,
}

/// A single streamed event from the model.
#[derive(Debug, Clone)]
pub enum ResponseEvent {
    /// A text delta streamed from the model
    TextDelta(String),
    /// The model wants to call a tool.
    ///
    /// OpenAI streams parallel tool calls interleaved by `index` - chunks for
    /// the same tool call share an index and must be accumulated separately.
    /// Providers that do not support parallel streaming (Anthropic, Cohere,
    /// AWS) always emit index 0.
    ToolCall {
        /// Parallel slot index (0-based).  Chunks belonging to the same tool
        /// call always carry the same index.
        index: u32,
        id: String,
        name: String,
        /// Partial JSON arguments for this chunk (accumulate across deltas)
        arguments: String,
    },
    /// A thinking/reasoning delta from the model (extended thinking API).
    /// Accumulated into a Thinking segment and collapsed by default in the UI.
    ThinkingDelta(String),
    /// Final usage statistics
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        /// Tokens served from the provider's prompt cache (read hit).
        cache_read_tokens: u32,
        /// Tokens written into the provider's prompt cache (write/creation).
        cache_write_tokens: u32,
        /// Cost in USD when reported by the API (e.g. OpenRouter `usage.cost`).
        cost_usd: Option<f64>,
    },
    /// The stream finished normally
    Done,
    /// The model stopped because it hit the output-token limit (`max_tokens`).
    ///
    /// This arrives *before* [`Done`] in the same stream.  The agent uses it
    /// to detect that any pending tool-call arguments were truncated, so that
    /// partial content can be recovered rather than silently discarded.
    MaxTokens,
    /// A structured error the provider sent as a stream event rather than an
    /// HTTP status, because the response had already started streaming when
    /// the failure happened. Terminates the turn.
    Error(String),
}

/// Token usage from one turn.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Tokens served from the provider's prompt cache (read hit).
    pub cache_read_tokens: u32,
    /// Tokens written into the provider's prompt cache (write/creation).
    pub cache_write_tokens: u32,
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Message construction ──────────────────────────────────────────────────

    #[test]
    fn message_user_sets_role_and_text() {
        let m = Message::user("hello");
        assert_eq!(m.role, Role::User);
        assert_eq!(m.as_text(), Some("hello"));
    }

    #[test]
    fn message_assistant_sets_role_and_text() {
        let m = Message::assistant("reply");
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.as_text(), Some("reply"));
    }

    #[test]
    fn message_system_sets_role_and_text() {
        let m = Message::system("prompt");
        assert_eq!(m.role, Role::System);
        assert_eq!(m.as_text(), Some("prompt"));
    }

    #[test]
    fn message_tool_result_sets_role_and_content() {
        let m = Message::tool_result("id-1", "output");
        assert_eq!(m.role, Role::Tool);
        assert!(m.as_text().is_none(), "tool_result has no text accessor");
        match &m.content {
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "id-1");
                assert_eq!(content.as_text(), Some("output"));
            }
            _ => panic!("wrong content variant"),
        }
    }

    #[test]
    fn message_tool_result_with_image_parts() {
        let parts = vec![
            ToolContentPart::Text {
                text: "here is the chart".into(),
            },
            ToolContentPart::Image {
                image_url: "data:image/png;base64,ABC".into(),
            },
        ];
        let m = Message::tool_result_with_parts("call-1", parts);
        assert_eq!(m.role, Role::Tool);
        assert_eq!(m.image_urls(), vec!["data:image/png;base64,ABC"]);
    }

    #[test]
    fn message_user_with_parts_image() {
        let parts = vec![
            ContentPart::Text {
                text: "what is this?".into(),
            },
            ContentPart::image("data:image/png;base64,XYZ"),
        ];
        let m = Message::user_with_parts(parts);
        assert_eq!(m.role, Role::User);
        assert_eq!(m.image_urls(), vec!["data:image/png;base64,XYZ"]);
        // as_text() is None for multi-part
        assert!(m.as_text().is_none());
    }

    #[test]
    fn as_text_returns_none_for_tool_call_content() {
        let m = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "x".into(),
                function: FunctionCall {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            },
        };
        assert!(m.as_text().is_none());
    }

    // ── Token approximation ───────────────────────────────────────────────────

    #[test]
    fn approx_tokens_text_divides_by_four() {
        let m = Message::user("12345678");
        assert_eq!(m.approx_tokens(), 2);
    }

    #[test]
    fn approx_tokens_minimum_is_one() {
        let m = Message::user("hi");
        assert_eq!(m.approx_tokens(), 1);
    }

    #[test]
    fn approx_tokens_empty_text_is_one() {
        let m = Message::user("");
        assert_eq!(m.approx_tokens(), 1);
    }

    #[test]
    fn approx_tokens_tool_call_uses_name_plus_args() {
        let m = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "id".into(),
                function: FunctionCall {
                    name: "aaaa".into(),          // 4 chars
                    arguments: "bbbbbbbb".into(), // 8 chars
                },
            },
        };
        // 12 chars / 4 = 3 tokens
        assert_eq!(m.approx_tokens(), 3);
    }

    #[test]
    fn approx_tokens_tool_result_uses_content() {
        let m = Message::tool_result("id", "1234567890123456"); // 16 chars → 4 tokens
        assert_eq!(m.approx_tokens(), 4);
    }

    #[test]
    fn approx_tokens_image_part_default_uses_high_estimate() {
        let parts = vec![ContentPart::image("data:image/png;base64,A")];
        let m = Message::user_with_parts(parts);
        assert_eq!(m.approx_tokens(), 765);
    }

    #[test]
    fn approx_tokens_image_detail_low_uses_85_tokens() {
        let parts = vec![ContentPart::image_with_detail(
            "data:image/png;base64,A",
            "low",
        )];
        let m = Message::user_with_parts(parts);
        assert_eq!(m.approx_tokens(), 85);
    }

    #[test]
    fn approx_tokens_image_detail_high_uses_765_tokens() {
        let parts = vec![ContentPart::image_with_detail(
            "data:image/png;base64,A",
            "high",
        )];
        let m = Message::user_with_parts(parts);
        assert_eq!(m.approx_tokens(), 765);
    }

    // ── Serialisation round-trip ──────────────────────────────────────────────

    #[test]
    fn message_serialises_and_deserialises() {
        let original = Message::user("test payload");
        let json = serde_json::to_string(&original).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.role, Role::User);
        assert_eq!(decoded.as_text(), Some("test payload"));
    }

    #[test]
    fn tool_schema_serialises_correctly() {
        let ts = ToolSchema {
            name: "my_tool".into(),
            description: "desc".into(),
            parameters: serde_json::json!({ "type": "object" }),
            ..Default::default()
        };
        let json = serde_json::to_string(&ts).unwrap();
        assert!(json.contains("my_tool"));
        assert!(json.contains("desc"));
    }

    #[test]
    fn tool_result_content_text_round_trip() {
        let c = ToolResultContent::Text("hello".into());
        let json = serde_json::to_string(&c).unwrap();
        let back: ToolResultContent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_text(), Some("hello"));
    }

    #[test]
    fn content_part_image_round_trip() {
        let p = ContentPart::image("data:image/png;base64,ABC");
        let json = serde_json::to_string(&p).unwrap();
        let back: ContentPart = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn content_part_image_with_detail_round_trip() {
        let p = ContentPart::image_with_detail("data:image/png;base64,ABC", "low");
        let json = serde_json::to_string(&p).unwrap();
        // "detail" field must be present in JSON
        assert!(
            json.contains("\"detail\""),
            "detail should be serialized when Some: {json}"
        );
        let back: ContentPart = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn content_part_image_without_detail_omits_field() {
        let p = ContentPart::image("data:image/png;base64,ABC");
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("\"detail\""),
            "detail should not appear when None: {json}"
        );
    }

    // ── Audio parts ───────────────────────────────────────────────────────────

    #[test]
    fn content_part_audio_round_trips() {
        let p = ContentPart::audio("data:audio/wav;base64,ABC");
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"type\":\"audio\""), "{json}");
        assert!(!json.contains("\"format\""), "format is None: {json}");
        let back: ContentPart = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn content_part_audio_with_format_round_trips() {
        let p = ContentPart::audio_with_format("data:audio/wav;base64,ABC", "wav");
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"format\":\"wav\""), "{json}");
        assert_eq!(serde_json::from_str::<ContentPart>(&json).unwrap(), p);
    }

    #[test]
    fn audio_format_prefers_the_explicit_value() {
        let p = ContentPart::audio_with_format("data:audio/mpeg;base64,A", "wav");
        assert_eq!(p.audio_format(), "wav");
    }

    #[test]
    fn audio_format_derives_wav_from_the_mime_subtype() {
        assert_eq!(
            ContentPart::audio("data:audio/wav;base64,A").audio_format(),
            "wav"
        );
        assert_eq!(
            ContentPart::audio("data:audio/x-wav;base64,A").audio_format(),
            "wav"
        );
    }

    #[test]
    fn audio_format_derives_mp3_from_audio_mpeg() {
        assert_eq!(
            ContentPart::audio("data:audio/mpeg;base64,A").audio_format(),
            "mp3"
        );
        assert_eq!(
            ContentPart::audio("data:audio/mp3;base64,A").audio_format(),
            "mp3"
        );
    }

    #[test]
    fn audio_format_falls_back_to_wav_for_unknown_urls() {
        assert_eq!(ContentPart::audio("https://x/a.ogg").audio_format(), "wav");
        assert_eq!(ContentPart::audio("https://x/a.mp3").audio_format(), "mp3");
    }

    #[test]
    fn audio_format_is_empty_for_non_audio_parts() {
        assert_eq!(ContentPart::text("hi").audio_format(), "");
        assert_eq!(
            ContentPart::image("data:image/png;base64,A").audio_format(),
            ""
        );
    }

    #[test]
    fn message_audio_urls_collects_audio_parts_only() {
        let m = Message::user_with_parts(vec![
            ContentPart::text("listen"),
            ContentPart::image("data:image/png;base64,IMG"),
            ContentPart::audio("data:audio/wav;base64,AUD"),
        ]);
        assert_eq!(m.audio_urls(), vec!["data:audio/wav;base64,AUD"]);
        assert_eq!(m.image_urls(), vec!["data:image/png;base64,IMG"]);
    }

    #[test]
    fn tool_result_audio_urls_are_collected() {
        let m = Message::tool_result_with_parts(
            "c1",
            vec![ToolContentPart::Audio {
                audio_url: "data:audio/wav;base64,T".into(),
            }],
        );
        assert_eq!(m.audio_urls(), vec!["data:audio/wav;base64,T"]);
    }

    #[test]
    fn approx_tokens_audio_part_scales_with_payload_size() {
        // ~10 s of 16-bit 16 kHz mono audio ≈ 320 000 bytes ≈ 426 667 base64 chars.
        let long = ContentPart::audio(format!("data:audio/wav;base64,{}", "A".repeat(426_667)));
        let short = ContentPart::audio("data:audio/wav;base64,AAAA");
        let long_tokens = Message::user_with_parts(vec![long]).approx_tokens();
        let short_tokens = Message::user_with_parts(vec![short]).approx_tokens();
        // Short clips sit on the 100-token floor rather than costing nothing.
        assert_eq!(short_tokens, 100, "floor applies to tiny clips");
        assert!(
            long_tokens > short_tokens,
            "longer audio must cost more: {long_tokens} vs {short_tokens}"
        );
    }

    #[test]
    fn tool_result_display_skips_audio_parts() {
        let content = ToolResultContent::Parts(vec![
            ToolContentPart::Text {
                text: "here".into(),
            },
            ToolContentPart::Audio {
                audio_url: "data:audio/wav;base64,A".into(),
            },
        ]);
        assert_eq!(content.to_string(), "here");
    }

    #[test]
    fn content_part_image_deserialises_without_detail_field() {
        // Ensure old serialized data (no detail field) still deserializes correctly.
        let json = r#"{"type":"image","image_url":"data:image/png;base64,ABC"}"#;
        let p: ContentPart = serde_json::from_str(json).unwrap();
        assert_eq!(p, ContentPart::image("data:image/png;base64,ABC"));
    }
}
