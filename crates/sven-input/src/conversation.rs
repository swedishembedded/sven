use serde::{Deserialize, Serialize};
use sven_model::{FunctionCall, Message, MessageContent, Role};

/// Parsed representation of a conversation markdown file.
#[derive(Debug, Default)]
pub struct ParsedConversation {
    /// Optional H1 title of the conversation.
    pub title: Option<String>,
    /// All complete turns that form the conversation history.
    /// This is ready to pass to `Agent::replace_history_and_submit`.
    pub history: Vec<Message>,
    /// If the file ends with a `## User` section that has no corresponding
    /// `## Sven` response, it is treated as pending input to execute.
    pub pending_user_input: Option<String>,
}

/// A raw H2 section parsed from the markdown file.
#[derive(Debug)]
struct Section {
    /// The heading text after `## `, e.g. "User", "Sven", "Tool", "Tool Result"
    heading: SectionKind,
    /// The raw content between this heading and the next.
    content: String,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum SectionKind {
    User,
    Sven,
    Tool,
    ToolResult,
    Unknown(String),
}

impl SectionKind {
    fn from_str(s: &str) -> Self {
        match s.trim() {
            "User" => SectionKind::User,
            "Sven" => SectionKind::Sven,
            "Tool" => SectionKind::Tool,
            "Tool Result" => SectionKind::ToolResult,
            other => SectionKind::Unknown(other.to_string()),
        }
    }
}

/// JSON envelope stored inside a `## Tool` section.
#[derive(Debug, Deserialize, Serialize)]
struct ToolCallEnvelope {
    pub tool_call_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Parse an error with context.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("orphaned ## Tool Result without a preceding ## Tool section")]
    OrphanedToolResult,
    #[error("## Tool section contains invalid JSON: {0}")]
    InvalidToolJson(String),
    #[error("## Tool section missing JSON code block")]
    MissingToolJson,
}

/// Parse a conversation markdown file into history messages and optional pending input.
///
/// The format uses H2 sections as turn boundaries:
///
/// ```markdown
/// # Optional title
///
/// ## User
/// Question or task here.
///
/// ## Sven
/// Agent response here.
///
/// ## Tool
/// ```json
/// {"tool_call_id": "call_001", "name": "read_file", "args": {"path": "/src/main.rs"}}
/// ```
///
/// ## Tool Result
/// ```text
/// file contents
/// ```
///
/// If the file ends with a `## User` section, it is returned as `pending_user_input`
/// and not included in `history`.
pub fn parse_conversation(markdown: &str) -> Result<ParsedConversation, ParseError> {
    let (title, sections) = split_sections(markdown);
    convert_sections_to_conversation(title, sections)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Split markdown into an optional H1 title and a list of H2 sections.
///
/// Only the four recognized section headings (`User`, `Sven`, `Tool`,
/// `Tool Result`) are treated as structural boundaries.  Any other `## …`
/// line — for example a heading the agent wrote inside its own response —
/// is kept as literal content within the current section.
fn split_sections(markdown: &str) -> (Option<String>, Vec<Section>) {
    let mut title: Option<String> = None;
    let mut sections: Vec<Section> = Vec::new();
    let mut current_heading: Option<SectionKind> = None;
    let mut current_content = String::new();
    let mut preamble = String::new();

    for line in markdown.lines() {
        // H1 — title, only before any section starts
        if let Some(h1) = line.strip_prefix("# ").filter(|_| !line.starts_with("## ")) {
            if current_heading.is_none() && preamble.is_empty() && title.is_none() {
                title = Some(h1.trim().to_string());
                continue;
            }
        }

        // H2 — only start a new section if the heading is a recognized kind
        if let Some(h2) = line.strip_prefix("## ") {
            let kind = SectionKind::from_str(h2.trim());
            if !matches!(kind, SectionKind::Unknown(_)) {
                // Flush previous section
                if let Some(heading) = current_heading.take() {
                    sections.push(Section {
                        heading,
                        content: current_content.trim_matches('\n').to_string(),
                    });
                } else {
                    preamble.push_str(&current_content);
                }
                current_content = String::new();
                current_heading = Some(kind);
                continue;
            }
            // Unknown H2 — fall through and treat as content
        }

        current_content.push_str(line);
        current_content.push('\n');
    }

    // Flush final section
    if let Some(heading) = current_heading {
        sections.push(Section {
            heading,
            content: current_content.trim_matches('\n').to_string(),
        });
    }

    (title, sections)
}

/// Convert a list of sections into a `ParsedConversation`.
fn convert_sections_to_conversation(
    title: Option<String>,
    sections: Vec<Section>,
) -> Result<ParsedConversation, ParseError> {
    let mut history: Vec<Message> = Vec::new();
    let mut pending_tool_call_id: Option<String> = None;
    let mut iter = sections.into_iter().peekable();

    while let Some(section) = iter.next() {
        match &section.heading {
            SectionKind::Unknown(name) => {
                tracing::warn!(heading = %name, "skipping unknown H2 section in conversation file");
            }

            SectionKind::User => {
                let content = section.content.clone();
                // If this is the last section and there is no next section,
                // we'll handle it after the loop.
                if iter.peek().is_none() {
                    // Last section — treat as pending input
                    return Ok(ParsedConversation {
                        title,
                        history,
                        pending_user_input: Some(content.trim().to_string()),
                    });
                }
                history.push(Message::user(content.trim()));
            }

            SectionKind::Sven => {
                history.push(Message::assistant(section.content.trim()));
            }

            SectionKind::Tool => {
                let envelope = parse_tool_envelope(&section.content)?;
                pending_tool_call_id = Some(envelope.tool_call_id.clone());
                history.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: envelope.tool_call_id,
                        function: FunctionCall {
                            name: envelope.name,
                            arguments: envelope.args.to_string(),
                        },
                    },
                });
            }

            SectionKind::ToolResult => {
                let call_id = match pending_tool_call_id.take() {
                    Some(id) => id,
                    None => return Err(ParseError::OrphanedToolResult),
                };
                let content = extract_code_block_content(&section.content);
                history.push(Message::tool_result(call_id, content.trim()));
            }
        }
    }

    Ok(ParsedConversation {
        title,
        history,
        pending_user_input: None,
    })
}

/// Parse a `## Tool` section body to extract the JSON envelope.
/// Expects a ```json code block anywhere in the content.
fn parse_tool_envelope(content: &str) -> Result<ToolCallEnvelope, ParseError> {
    let json_str = extract_fenced_block(content, "json")
        .or_else(|| extract_fenced_block(content, ""))
        .ok_or(ParseError::MissingToolJson)?;

    serde_json::from_str::<ToolCallEnvelope>(&json_str)
        .map_err(|e| ParseError::InvalidToolJson(e.to_string()))
}

/// Extract the content of the first fenced code block with the given language tag.
/// If `lang` is empty, matches any code fence (` ``` ` with no language).
fn extract_fenced_block(content: &str, lang: &str) -> Option<String> {
    let open_marker = if lang.is_empty() {
        "```".to_string()
    } else {
        format!("```{lang}")
    };

    let mut in_block = false;
    let mut result = String::new();

    for line in content.lines() {
        if !in_block {
            let trimmed = line.trim();
            if lang.is_empty() {
                // Match ``` with no language suffix (just whitespace/end)
                if trimmed == "```" {
                    in_block = true;
                    continue;
                }
            } else if trimmed == open_marker.as_str() || trimmed.starts_with(&format!("{open_marker} ")) {
                in_block = true;
                continue;
            }
        } else {
            if line.trim() == "```" {
                return Some(result);
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }

    if in_block { Some(result) } else { None }
}

/// Extract the content of a code block from section body (for Tool Result).
/// If the content is wrapped in a code fence, return the inner text; otherwise
/// return the full content as-is.
fn extract_code_block_content(content: &str) -> String {
    if let Some(inner) = extract_fenced_block(content, "") {
        return inner;
    }
    // Try any language-tagged block
    for line in content.lines() {
        if line.trim().starts_with("```") && line.trim().len() > 3 {
            let lang = line.trim().trim_start_matches('`').trim();
            if let Some(inner) = extract_fenced_block(content, lang) {
                return inner;
            }
        }
    }
    content.to_string()
}

// ── Serializer ────────────────────────────────────────────────────────────────

/// Serialize a slice of messages into conversation markdown sections.
///
/// The output is suitable for appending to an existing conversation file.
/// System messages are skipped (they are injected by the agent automatically).
pub fn serialize_conversation_turn(messages: &[Message]) -> String {
    let mut result = String::new();
    for msg in messages {
        result.push_str(&message_to_section(msg));
    }
    result
}

/// Serialize the entire conversation into a fresh markdown file, including
/// the conversation title if provided.
pub fn serialize_conversation(title: Option<&str>, messages: &[Message]) -> String {
    let mut result = String::new();
    if let Some(t) = title {
        result.push_str(&format!("# {t}\n\n"));
    }
    result.push_str(&serialize_conversation_turn(messages));
    result
}

fn message_to_section(msg: &Message) -> String {
    match (&msg.role, &msg.content) {
        (Role::System, _) => String::new(), // skip — agent injects system message

        (Role::User, MessageContent::Text(t)) => {
            format!("## User\n{}\n\n", t.trim())
        }

        (Role::Assistant, MessageContent::Text(t)) => {
            format!("## Sven\n{}\n\n", t.trim())
        }

        (Role::Assistant, MessageContent::ToolCall { tool_call_id, function }) => {
            let args_value: serde_json::Value =
                serde_json::from_str(&function.arguments).unwrap_or(serde_json::Value::Null);
            let envelope = serde_json::json!({
                "tool_call_id": tool_call_id,
                "name": function.name,
                "args": args_value,
            });
            let pretty = serde_json::to_string_pretty(&envelope).unwrap_or_default();
            format!("## Tool\n```json\n{pretty}\n```\n\n")
        }

        (Role::Tool, MessageContent::ToolResult { content, .. }) => {
            format!("## Tool Result\n```\n{content}\n```\n\n")
        }

        _ => String::new(),
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(t: &str) -> Message { Message::user(t) }
    fn sven_msg(t: &str) -> Message { Message::assistant(t) }

    // ── Parsing ───────────────────────────────────────────────────────────────

    #[test]
    fn parse_simple_exchange() {
        let md = "## User\nHello\n\n## Sven\nHi there\n";
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        assert_eq!(conv.history.len(), 2);
        assert_eq!(conv.history[0].as_text(), Some("Hello"));
        assert_eq!(conv.history[1].as_text(), Some("Hi there"));
    }

    #[test]
    fn parse_with_optional_title() {
        let md = "# My Project\n\n## User\nDo work\n\n## Sven\nDone\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.title.as_deref(), Some("My Project"));
        assert_eq!(conv.history.len(), 2);
    }

    #[test]
    fn pending_user_input_when_last_section_is_user() {
        let md = "## User\nFirst task\n\n## Sven\nDone\n\n## User\nSecond task\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.pending_user_input.as_deref(), Some("Second task"));
        assert_eq!(conv.history.len(), 2);
    }

    #[test]
    fn no_pending_input_when_last_section_is_sven() {
        let md = "## User\nTask\n\n## Sven\nResponse\n";
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        assert_eq!(conv.history.len(), 2);
    }

    #[test]
    fn parse_tool_call_and_result() {
        let md = concat!(
            "## User\nSearch files\n\n",
            "## Sven\nI'll search\n\n",
            "## Tool\n```json\n{\"tool_call_id\":\"call_1\",\"name\":\"glob\",\"args\":{\"pattern\":\"**/*.rs\"}}\n```\n\n",
            "## Tool Result\n```\nsrc/main.rs\n```\n\n",
            "## Sven\nFound main.rs\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        // User, Sven("I'll search"), ToolCall, ToolResult, Sven("Found main.rs") = 5
        assert_eq!(conv.history.len(), 5);

        // Check Tool call at index 2
        match &conv.history[2].content {
            MessageContent::ToolCall { tool_call_id, function } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(function.name, "glob");
            }
            _ => panic!("expected ToolCall"),
        }

        // Check Tool result at index 3
        match &conv.history[3].content {
            MessageContent::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(content.trim(), "src/main.rs");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn orphaned_tool_result_returns_error() {
        let md = "## User\nTask\n\n## Tool Result\n```\noutput\n```\n";
        let err = parse_conversation(md).unwrap_err();
        assert!(matches!(err, ParseError::OrphanedToolResult));
    }

    #[test]
    fn nested_code_block_in_sven_section_does_not_break_parsing() {
        let md = concat!(
            "## User\nHow to write Rust?\n\n",
            "## Sven\nHere is an example:\n```rust\nfn main() {}\n```\nDone.\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.history.len(), 2);
        let response = conv.history[1].as_text().unwrap();
        assert!(response.contains("```rust"));
        assert!(response.contains("fn main()"));
    }

    #[test]
    fn unknown_h2_inside_sven_section_is_treated_as_content() {
        // A Sven response that uses ## sub-headings must not be split/truncated.
        let md = concat!(
            "## User\nWhat's the plan?\n\n",
            "## Sven\nHere is the plan:\n## Phase 1\nDo this first.\n## Phase 2\nThen do that.\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        assert_eq!(conv.history.len(), 2, "unknown h2 must stay inside the Sven section");
        let body = conv.history[1].as_text().unwrap();
        assert!(body.contains("## Phase 1"), "Phase 1 preserved");
        assert!(body.contains("## Phase 2"), "Phase 2 preserved");
        assert!(body.contains("Do this first"), "Phase 1 body preserved");
        assert!(body.contains("Then do that"), "Phase 2 body preserved");
    }

    #[test]
    fn empty_conversation_returns_empty() {
        let conv = parse_conversation("").unwrap();
        assert!(conv.title.is_none());
        assert!(conv.history.is_empty());
        assert!(conv.pending_user_input.is_none());
    }

    #[test]
    fn file_with_only_user_section_is_pending_input() {
        let md = "## User\nInitial task\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.pending_user_input.as_deref(), Some("Initial task"));
        assert!(conv.history.is_empty());
    }

    // ── Serializer ────────────────────────────────────────────────────────────

    #[test]
    fn serialize_user_message() {
        let msg = user_msg("Hello agent");
        let out = message_to_section(&msg);
        assert!(out.starts_with("## User\n"));
        assert!(out.contains("Hello agent"));
    }

    #[test]
    fn serialize_assistant_message() {
        let msg = sven_msg("Here's my response");
        let out = message_to_section(&msg);
        assert!(out.starts_with("## Sven\n"));
        assert!(out.contains("Here's my response"));
    }

    #[test]
    fn serialize_tool_call() {
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_abc".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"/tmp/x"}"#.into(),
                },
            },
        };
        let out = message_to_section(&msg);
        assert!(out.starts_with("## Tool\n"));
        assert!(out.contains("```json"));
        assert!(out.contains("call_abc"));
        assert!(out.contains("read_file"));
    }

    #[test]
    fn serialize_tool_result() {
        let msg = Message::tool_result("call_abc", "file contents");
        let out = message_to_section(&msg);
        assert!(out.starts_with("## Tool Result\n"));
        assert!(out.contains("file contents"));
        assert!(out.contains("```"));
    }

    #[test]
    fn system_messages_are_skipped() {
        let msg = Message::system("You are a helpful assistant");
        let out = message_to_section(&msg);
        assert!(out.is_empty());
    }

    // ── Round-trip ────────────────────────────────────────────────────────────

    #[test]
    fn round_trip_user_sven() {
        let messages = vec![user_msg("Do task"), sven_msg("Task done")];
        let md = serialize_conversation_turn(&messages);
        let conv = parse_conversation(&md).unwrap();
        assert_eq!(conv.history.len(), 2);
        assert_eq!(conv.history[0].as_text(), Some("Do task"));
        assert_eq!(conv.history[1].as_text(), Some("Task done"));
    }

    #[test]
    fn round_trip_with_tool_call() {
        let messages = vec![
            user_msg("Search"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "id_1".into(),
                    function: FunctionCall {
                        name: "glob".into(),
                        arguments: r#"{"pattern":"**/*.rs"}"#.into(),
                    },
                },
            },
            Message::tool_result("id_1", "src/main.rs"),
            sven_msg("Found main.rs"),
        ];
        let md = serialize_conversation_turn(&messages);
        let conv = parse_conversation(&md).unwrap();
        // Last section is "## Sven" so no pending input
        assert!(conv.pending_user_input.is_none());
        assert_eq!(conv.history.len(), 4);
        match &conv.history[1].content {
            MessageContent::ToolCall { tool_call_id, function } => {
                assert_eq!(tool_call_id, "id_1");
                assert_eq!(function.name, "glob");
            }
            _ => panic!("expected ToolCall"),
        }
        match &conv.history[2].content {
            MessageContent::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "id_1");
                assert_eq!(content.trim(), "src/main.rs");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn round_trip_trailing_user_becomes_pending() {
        let md = "## User\nDo task\n\n## Sven\nDone\n\n## User\nContinue\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.pending_user_input.as_deref(), Some("Continue"));
        assert_eq!(conv.history.len(), 2);
    }

    // ── Error paths ───────────────────────────────────────────────────────────

    #[test]
    fn tool_section_missing_json_returns_error() {
        let md = "## User\nTask\n\n## Tool\nNo code block here\n\n## Tool Result\n```\nout\n```\n";
        let err = parse_conversation(md).unwrap_err();
        assert!(matches!(err, ParseError::MissingToolJson));
    }

    #[test]
    fn tool_section_invalid_json_returns_error() {
        let md = "## User\nTask\n\n## Tool\n```json\nnot json at all\n```\n\n## Tool Result\n```\nout\n```\n";
        let err = parse_conversation(md).unwrap_err();
        assert!(matches!(err, ParseError::InvalidToolJson(_)));
    }

    #[test]
    fn tool_section_json_missing_required_fields_returns_error() {
        // JSON is valid but missing tool_call_id / name
        let md = "## User\nTask\n\n## Tool\n```json\n{\"foo\":\"bar\"}\n```\n\n## Tool Result\n```\nout\n```\n";
        let err = parse_conversation(md).unwrap_err();
        assert!(matches!(err, ParseError::InvalidToolJson(_)));
    }

    // ── Tool Result edge cases ────────────────────────────────────────────────

    #[test]
    fn tool_result_without_code_fence_uses_raw_content() {
        let md = concat!(
            "## User\nRun it\n\n",
            "## Tool\n```json\n{\"tool_call_id\":\"c1\",\"name\":\"shell\",\"args\":{}}\n```\n\n",
            "## Tool Result\nplain output line\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.history.len(), 3);
        match &conv.history[2].content {
            MessageContent::ToolResult { content, .. } => {
                assert!(content.contains("plain output line"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn tool_result_content_with_backticks_inside_fence() {
        // Backtick-containing output wrapped in a fenced block
        let md = concat!(
            "## User\nTask\n\n",
            "## Tool\n```json\n{\"tool_call_id\":\"c2\",\"name\":\"shell\",\"args\":{}}\n```\n\n",
            "## Tool Result\n```\nline with `backticks` inside\n```\n",
        );
        let conv = parse_conversation(md).unwrap();
        match &conv.history[2].content {
            MessageContent::ToolResult { content, .. } => {
                assert!(content.contains("backticks"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    // ── Multiple tool calls ───────────────────────────────────────────────────

    #[test]
    fn multiple_sequential_tool_calls_with_user() {
        let md = concat!(
            "## User\nSearch and read\n\n",
            "## Tool\n```json\n{\"tool_call_id\":\"c1\",\"name\":\"glob\",\"args\":{\"p\":\"*.rs\"}}\n```\n\n",
            "## Tool Result\n```\nsrc/main.rs\n```\n\n",
            "## Tool\n```json\n{\"tool_call_id\":\"c2\",\"name\":\"read_file\",\"args\":{\"path\":\"src/main.rs\"}}\n```\n\n",
            "## Tool Result\n```\nfn main() {}\n```\n\n",
            "## Sven\nDone\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        // User + TC1 + TR1 + TC2 + TR2 + Sven = 6
        assert_eq!(conv.history.len(), 6);

        // Verify IDs are preserved correctly
        match &conv.history[1].content {
            MessageContent::ToolCall { tool_call_id, function } => {
                assert_eq!(tool_call_id, "c1");
                assert_eq!(function.name, "glob");
            }
            _ => panic!("expected first ToolCall at [1]"),
        }
        match &conv.history[2].content {
            MessageContent::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "c1");
                assert!(content.contains("src/main.rs"));
            }
            _ => panic!("expected first ToolResult at [2]"),
        }
        match &conv.history[3].content {
            MessageContent::ToolCall { tool_call_id, function } => {
                assert_eq!(tool_call_id, "c2");
                assert_eq!(function.name, "read_file");
            }
            _ => panic!("expected second ToolCall at [3]"),
        }
        match &conv.history[4].content {
            MessageContent::ToolResult { tool_call_id, .. } => {
                assert_eq!(tool_call_id, "c2");
            }
            _ => panic!("expected second ToolResult at [4]"),
        }
    }

    // ── Multiline content preservation ───────────────────────────────────────

    #[test]
    fn multiline_user_input_preserved() {
        let md = "## User\nLine one\nLine two\nLine three\n\n## Sven\nOk\n";
        let conv = parse_conversation(md).unwrap();
        let text = conv.history[0].as_text().unwrap();
        assert!(text.contains("Line one"));
        assert!(text.contains("Line two"));
        assert!(text.contains("Line three"));
    }

    #[test]
    fn blank_lines_inside_sven_response_preserved() {
        let md = "## User\nTask\n\n## Sven\nParagraph one.\n\nParagraph two.\n";
        let conv = parse_conversation(md).unwrap();
        let text = conv.history[1].as_text().unwrap();
        assert!(text.contains("Paragraph one"));
        assert!(text.contains("Paragraph two"));
    }

    // ── Serializer extras ─────────────────────────────────────────────────────

    #[test]
    fn serialize_conversation_with_title() {
        let messages = vec![user_msg("Hi"), sven_msg("Hello")];
        let md = serialize_conversation(Some("My Session"), &messages);
        assert!(md.starts_with("# My Session\n\n"), "title present at top");
        assert!(md.contains("## User"), "user section follows");
    }

    #[test]
    fn serialize_conversation_without_title() {
        let messages = vec![user_msg("Hi"), sven_msg("Hello")];
        let md = serialize_conversation(None, &messages);
        assert!(!md.starts_with("# "), "no H1 title line");
        assert!(md.starts_with("## "), "starts directly with a section");
    }

    #[test]
    fn serialize_tool_args_are_pretty_printed() {
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "id".into(),
                function: FunctionCall {
                    name: "fn".into(),
                    arguments: r#"{"a":1,"b":2}"#.into(),
                },
            },
        };
        let out = message_to_section(&msg);
        // Args are nested inside the "args" envelope key so indented 4 spaces
        assert!(out.contains("\"a\": 1"), "arg key a is present");
        assert!(out.contains("\"b\": 2"), "arg key b is present");
        // Verify it is actually pretty-printed (newlines between keys)
        assert!(out.contains("{\n"), "pretty-printed JSON has newlines");
    }

    // ── Append simulation ─────────────────────────────────────────────────────

    /// Simulate the full iterative workflow:
    /// 1. Initial file: User A → Sven A
    /// 2. User appends "## User\nSecond request" to the file
    /// 3. sven parses → gets history=[User A, Sven A], pending="Second request"
    /// 4. sven produces Sven B and appends it
    /// 5. Parse the final file → history has all 4 messages, no pending
    #[test]
    fn iterative_append_workflow() {
        let initial = "## User\nFirst task\n\n## Sven\nDone.\n";

        // Step 1: parse initial file (no pending)
        let conv1 = parse_conversation(initial).unwrap();
        assert!(conv1.pending_user_input.is_none());
        assert_eq!(conv1.history.len(), 2);

        // Step 2: user appends a new User section
        let after_user_append = format!("{initial}\n## User\nSecond request\n");

        // Step 3: parse after user appended
        let conv2 = parse_conversation(&after_user_append).unwrap();
        assert_eq!(conv2.pending_user_input.as_deref(), Some("Second request"));
        assert_eq!(conv2.history.len(), 2, "only the completed turn in history");

        // Step 4: agent produces a response, serialize and append
        let agent_response = vec![sven_msg("Second task done.")];
        let to_append = serialize_conversation_turn(&agent_response);
        let final_file = format!("{after_user_append}{to_append}");

        // Step 5: parse the final file
        let conv3 = parse_conversation(&final_file).unwrap();
        assert!(conv3.pending_user_input.is_none());
        assert_eq!(conv3.history.len(), 4, "all four turns in history");
        assert_eq!(conv3.history[0].as_text(), Some("First task"));
        assert_eq!(conv3.history[1].as_text(), Some("Done."));
        assert_eq!(conv3.history[2].as_text(), Some("Second request"));
        assert_eq!(conv3.history[3].as_text(), Some("Second task done."));
    }

    // ── Whitespace edge cases ─────────────────────────────────────────────────

    #[test]
    fn section_content_is_trimmed_of_surrounding_newlines() {
        // Extra blank lines around content must not change what gets stored
        let md = "## User\n\nHello\n\n\n\n## Sven\n\nWorld\n\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.history[0].as_text(), Some("Hello"));
        assert_eq!(conv.history[1].as_text(), Some("World"));
    }

    #[test]
    fn trailing_whitespace_in_section_heading_is_handled() {
        // "## User  " (trailing spaces) — should still be recognized
        let md = "## User  \nHello\n\n## Sven\nOk\n";
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.history.len(), 2);
    }
}
