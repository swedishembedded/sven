use serde::{Deserialize, Serialize};

/// A single message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: MessageContent::Text(text.into()) }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: MessageContent::Text(text.into()) }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: MessageContent::Text(text.into()) }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: id.into(),
                content: content.into(),
            },
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn approx_tokens(&self) -> usize {
        let chars = match &self.content {
            MessageContent::Text(t) => t.len(),
            MessageContent::ToolCall { function, .. } => {
                function.name.len() + function.arguments.len()
            }
            MessageContent::ToolResult { content, .. } => content.len(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    ToolCall {
        tool_call_id: String,
        function: FunctionCall,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded argument object
    pub arguments: String,
}

/// A tool schema provided to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema of the parameters object
    pub parameters: serde_json::Value,
}

/// Request sent to a model provider.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub stream: bool,
}

/// A single streamed event from the model.
#[derive(Debug, Clone)]
pub enum ResponseEvent {
    /// A text delta streamed from the model
    TextDelta(String),
    /// The model wants to call a tool
    ToolCall {
        id: String,
        name: String,
        /// Accumulated JSON arguments (may arrive across multiple deltas)
        arguments: String,
    },
    /// A thinking/reasoning delta from the model (extended thinking API).
    /// Accumulated into a Thinking segment and collapsed by default in the UI.
    ThinkingDelta(String),
    /// Final usage statistics
    Usage { input_tokens: u32, output_tokens: u32 },
    /// The stream finished normally
    Done,
    /// A recoverable error (non-fatal warning)
    Error(String),
}

/// Token usage from one turn.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
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
            MessageContent::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "id-1");
                assert_eq!(content, "output");
            }
            _ => panic!("wrong content variant"),
        }
    }

    #[test]
    fn as_text_returns_none_for_tool_call_content() {
        let m = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "x".into(),
                function: FunctionCall { name: "f".into(), arguments: "{}".into() },
            },
        };
        assert!(m.as_text().is_none());
    }

    // ── Token approximation ───────────────────────────────────────────────────

    #[test]
    fn approx_tokens_text_divides_by_four() {
        // 8 chars → 2 tokens
        let m = Message::user("12345678");
        assert_eq!(m.approx_tokens(), 2);
    }

    #[test]
    fn approx_tokens_minimum_is_one() {
        // Very short text should still return 1
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
                    name: "aaaa".into(),       // 4 chars
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
        };
        let json = serde_json::to_string(&ts).unwrap();
        assert!(json.contains("my_tool"));
        assert!(json.contains("desc"));
    }
}
