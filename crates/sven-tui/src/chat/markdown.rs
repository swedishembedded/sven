// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Conversation ↔ Markdown: serialise `ChatSegment`s to the display-markdown
//! format used by the Neovim buffer, and parse that format back to `Message`s
//! for edit-and-resubmit.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::TodoItem;

use crate::chat::segment::ChatSegment;
use crate::markdown::StyledLines;

// ── Format helpers ────────────────────────────────────────────────────────────

/// Format todo items as markdown for conversation display.
pub fn format_todos_markdown(todos: &[TodoItem]) -> String {
    let mut result = String::from("\n**Todo List Updated:**\n\n");
    for todo in todos {
        result.push_str(&format!(
            "- **{}**: {} (id: {})\n",
            todo.status.to_uppercase(),
            todo.content,
            todo.id
        ));
    }
    result.push('\n');
    result
}

/// Format a single `ChatSegment` as markdown for display.
pub fn segment_to_markdown(seg: &ChatSegment, tool_args_cache: &HashMap<String, String>) -> String {
    match seg {
        ChatSegment::Message(m) => message_to_markdown(m, tool_args_cache),
        ChatSegment::ContextCompacted { tokens_before, tokens_after, strategy, turn } => {
            use sven_core::CompactionStrategyUsed;
            let label = match strategy {
                CompactionStrategyUsed::Structured => "Context compacted (structured)",
                CompactionStrategyUsed::Narrative => "Context compacted (narrative)",
                CompactionStrategyUsed::Emergency => "⚠ Context emergency-compacted",
            };
            let turn_note = if *turn > 0 {
                format!(" · tool round {turn}")
            } else {
                String::new()
            };
            format!(
                "\n---\n*{label}: {tokens_before} → {tokens_after} tokens{turn_note}*\n\n"
            )
        }
        ChatSegment::Error(msg) => format!("\n**Error**: {msg}\n\n"),
        ChatSegment::Thinking { content } => {
            format!("\n**Agent:thinking**\n💭 **Thought**\n```\n{}\n```\n", content)
        }
    }
}

/// Render a single-line collapsed preview for a segment (ratatui-only mode).
pub fn collapsed_preview(seg: &ChatSegment, tool_args_cache: &HashMap<String, String>) -> String {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            // User / assistant text: first line, up to 80 chars
            (Role::User, MessageContent::Text(t)) => {
                let first = t.lines().next().unwrap_or("").trim();
                let preview: String = first.chars().take(80).collect();
                let has_more = first.chars().count() > 80 || t.contains('\n');
                let ellipsis = if has_more { "…" } else { "" };
                format!("\n**User:** `{preview}{ellipsis}` ▶ click to expand\n")
            }
            (Role::Assistant, MessageContent::Text(t)) => {
                let first = t.lines().next().unwrap_or("").trim();
                let preview: String = first.chars().take(80).collect();
                let has_more = first.chars().count() > 80 || t.contains('\n');
                let ellipsis = if has_more { "…" } else { "" };
                format!("\n**Agent:** `{preview}{ellipsis}` ▶ click to expand\n")
            }
            (Role::Assistant, MessageContent::ToolCall { tool_call_id, function }) => {
                let args_preview = serde_json::from_str::<serde_json::Value>(&function.arguments)
                    .map(|v| {
                        if let serde_json::Value::Object(map) = &v {
                            let parts: Vec<String> = map.iter().take(2).map(|(k, val)| {
                                let s = match val {
                                    serde_json::Value::String(s) =>
                                        s.chars().take(40).collect::<String>(),
                                    other =>
                                        other.to_string().chars().take(40).collect::<String>(),
                                };
                                format!("{}={}", k, s)
                            }).collect();
                            parts.join(" ")
                        } else {
                            function.arguments.chars().take(60).collect::<String>()
                        }
                    })
                    .unwrap_or_else(|_| function.arguments.chars().take(60).collect::<String>());
                format!(
                    "\n**Agent:tool_call:{}**\n🔧 **Tool Call: {}** `{}` ▶ click to expand\n",
                    tool_call_id, function.name, args_preview
                )
            }
            (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
                let tool_name = tool_args_cache
                    .get(tool_call_id)
                    .map(|s| s.as_str())
                    .unwrap_or("tool");
                let content_text = content.to_string();
                let preview: String =
                    content_text.lines().next().unwrap_or("").chars().take(80).collect();
                let truncated = if content_text.len() > preview.len() + 1 { "…" } else { "" };
                format!(
                    "\n**Tool:{}**\n✅ **Tool Response: {}** `{}{}` ▶ click to expand\n",
                    tool_call_id, tool_name, preview, truncated
                )
            }
            _ => segment_to_markdown(seg, tool_args_cache),
        },
        ChatSegment::Thinking { content } => {
            let preview: String =
                content.lines().next().unwrap_or("").chars().take(80).collect();
            let truncated = if content.len() > preview.len() + 1 { "…" } else { "" };
            format!(
                "\n**Agent:thinking**\n💭 **Thought** `{}{}` ▶ click to expand\n",
                preview, truncated
            )
        }
        _ => segment_to_markdown(seg, tool_args_cache),
    }
}

/// Format the full conversation as a single markdown string (used for the
/// Neovim buffer content).
pub fn format_conversation(
    segments: &[ChatSegment],
    streaming_buffer: &str,
    tool_args_cache: &HashMap<String, String>,
) -> String {
    let mut result = String::new();
    for (i, seg) in segments.iter().enumerate() {
        let md = segment_to_markdown(seg, tool_args_cache);
        if i == 0 && md.starts_with('\n') {
            result.push_str(md.trim_start_matches('\n'));
        } else {
            result.push_str(&md);
        }
    }
    if !streaming_buffer.is_empty() {
        result.push_str("**Agent:** ");
        result.push_str(streaming_buffer);
    }
    result
}

/// Return `(bar_style, dim)` for a segment used to draw the per-segment colour
/// bar in the ratatui-only chat pane.
pub fn segment_bar_style(seg: &ChatSegment) -> (Option<Style>, bool) {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::User, MessageContent::Text(_)) =>
                (Some(Style::default().fg(Color::Green)), false),
            (Role::Assistant, MessageContent::Text(_)) =>
                (Some(Style::default().fg(Color::Blue)), false),
            (Role::Assistant, MessageContent::ToolCall { .. }) =>
                (Some(Style::default().fg(Color::Yellow)), false),
            (Role::Tool, MessageContent::ToolResult { .. }) =>
                (Some(Style::default().fg(Color::Yellow)), false),
            _ => (None, false),
        },
        ChatSegment::Thinking { .. } =>
            (Some(Style::default().fg(Color::Magenta)), false),
        ChatSegment::Error(_) =>
            (Some(Style::default().fg(Color::Red)), false),
        _ => (None, false),
    }
}

/// Prepend a coloured bar to every line and optionally apply `DIM` to content.
pub fn apply_bar_and_dim(
    lines: StyledLines,
    bar_style: Option<Style>,
    dim: bool,
    bar_char: &str,
) -> StyledLines {
    let modifier = if dim { Modifier::DIM } else { Modifier::empty() };
    lines
        .into_iter()
        .map(|line| {
            let mut spans = Vec::new();
            if let Some(style) = bar_style {
                spans.push(Span::styled(bar_char.to_string(), style));
            }
            for s in line.spans {
                spans.push(Span::styled(
                    s.content.to_string(),
                    s.style.patch(Style::default().add_modifier(modifier)),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// Format a single `Message` as markdown.  This is the per-message building
/// block for `format_conversation` and the round-trip parse tests.
pub(crate) fn message_to_markdown(m: &Message, tool_args_cache: &HashMap<String, String>) -> String {
    match (&m.role, &m.content) {
        (Role::User, MessageContent::Text(t)) =>
            format!("---\n\n**You:** {}\n", t),
        (Role::Assistant, MessageContent::Text(t)) =>
            format!("\n**Agent:** {}\n", t),
        (Role::Assistant, MessageContent::ToolCall { tool_call_id, function }) => {
            let pretty_args = serde_json::from_str::<serde_json::Value>(&function.arguments)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_else(|_| function.arguments.clone());
            format!(
                "\n**Agent:tool_call:{}**\n🔧 **Tool Call: {}**\n```json\n{}\n```\n",
                tool_call_id, function.name, pretty_args
            )
        }
        (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
            let tool_name = tool_args_cache
                .get(tool_call_id)
                .map(|s| s.as_str())
                .unwrap_or("tool");
            format!(
                "\n**Tool:{}**\n✅ **Tool Response: {}**\n```\n{}\n```\n",
                tool_call_id, tool_name, content
            )
        }
        (Role::System, MessageContent::Text(t)) =>
            format!("**System:** {}\n\n", t),
        _ => String::new(),
    }
}

// ── Parse helpers ─────────────────────────────────────────────────────────────

/// Parse a markdown buffer back into structured `Message`s for resubmit.
///
/// This is the inverse of `format_conversation` / `message_to_markdown`,
/// enabling lossless round-trip editing in the Neovim buffer.
pub fn parse_markdown_to_messages(markdown: &str) -> Result<Vec<Message>, String> {
    let mut messages = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line == "---" {
            i += 1;
            continue;
        }
        if let Some(msg) = parse_message_at_line(&lines, &mut i)? {
            messages.push(msg);
        } else {
            i += 1;
        }
    }
    Ok(messages)
}

fn parse_message_at_line(lines: &[&str], i: &mut usize) -> Result<Option<Message>, String> {
    if *i >= lines.len() {
        return Ok(None);
    }
    let line = lines[*i].trim();

    if line.starts_with("**You:**") {
        let text = extract_text_content(lines, i, "**You:**")?;
        return Ok(Some(Message::user(text)));
    }

    if line.starts_with("**Agent:tool_call:") {
        let tool_call_id = line
            .strip_prefix("**Agent:tool_call:")
            .and_then(|s| s.strip_suffix("**"))
            .ok_or_else(|| format!("Malformed tool_call header: {}", line))?
            .trim()
            .to_string();
        *i += 1;
        skip_until_code_fence(lines, i);
        let arguments = extract_code_block(lines, i)?;
        let name = extract_tool_name_from_previous_lines(lines, *i)?;
        return Ok(Some(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id,
                function: FunctionCall { name, arguments },
            },
        }));
    }

    if line.starts_with("**Tool:") {
        let tool_call_id = line
            .strip_prefix("**Tool:")
            .and_then(|s| s.strip_suffix("**"))
            .ok_or_else(|| format!("Malformed tool result header: {}", line))?
            .trim()
            .to_string();
        *i += 1;
        skip_until_code_fence(lines, i);
        let content = extract_code_block(lines, i)?;
        return Ok(Some(Message {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id,
                content: sven_model::ToolResultContent::Text(content),
            },
        }));
    }

    if line.starts_with("**Agent:**") {
        let text = extract_text_content(lines, i, "**Agent:**")?;
        return Ok(Some(Message::assistant(text)));
    }

    if line.starts_with("**System:**") {
        let text = extract_text_content(lines, i, "**System:**")?;
        return Ok(Some(Message::system(text)));
    }

    Ok(None)
}

fn extract_text_content(lines: &[&str], i: &mut usize, prefix: &str) -> Result<String, String> {
    let first_line = lines[*i].trim();
    let inline_text = first_line.strip_prefix(prefix).map(|s| s.trim()).unwrap_or("");
    let mut text = String::from(inline_text);
    *i += 1;
    // Track consecutive blank lines so they can be reproduced faithfully.
    // Trailing blank lines are discarded (never flushed after the last non-blank
    // line before a section header or end of input).
    let mut pending_blanks: usize = 0;
    while *i < lines.len() {
        let line = lines[*i];
        let trimmed = line.trim();
        if is_message_header(trimmed) {
            break;
        }
        if trimmed.is_empty() {
            pending_blanks += 1;
        } else {
            if !text.is_empty() {
                // One '\n' for the normal line separator, then one per blank line.
                text.push('\n');
                for _ in 0..pending_blanks {
                    text.push('\n');
                }
            }
            pending_blanks = 0;
            text.push_str(trimmed);
        }
        *i += 1;
    }
    Ok(text)
}

/// Returns true when `line` is a message-section delimiter that should stop
/// continuation-text accumulation.  Only the known role-header patterns and
/// the `---` turn separator qualify; generic `**bold**` markdown does NOT.
fn is_message_header(line: &str) -> bool {
    line == "---"
        || line.starts_with("**You:")
        || line.starts_with("**Agent:")
        || line.starts_with("**Tool:")
        || line.starts_with("**System:")
}

fn skip_until_code_fence(lines: &[&str], i: &mut usize) {
    while *i < lines.len() {
        if lines[*i].trim().starts_with("```") {
            return;
        }
        *i += 1;
    }
}

fn extract_code_block(lines: &[&str], i: &mut usize) -> Result<String, String> {
    if *i >= lines.len() || !lines[*i].trim().starts_with("```") {
        return Err(format!("Expected code fence at line {}", i));
    }
    *i += 1;
    let mut content = String::new();
    while *i < lines.len() {
        let line = lines[*i];
        if line.trim().starts_with("```") {
            *i += 1;
            return Ok(content.trim_end().to_string());
        }
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(line);
        *i += 1;
    }
    Err("Unclosed code block".to_string())
}

/// Scans backward from `current` to find the `🔧 **Tool Call: name**` display
/// line.  Bounded by section separators so it never bleeds into a prior segment.
fn extract_tool_name_from_previous_lines(lines: &[&str], current: usize) -> Result<String, String> {
    for j in (0..current).rev() {
        let line = lines[j].trim();
        if let Some(rest) = line.strip_prefix("🔧 **Tool Call:") {
            if let Some(name) = rest.strip_suffix("**") {
                return Ok(name.trim().to_string());
            }
        }
        if line == "---"
            || line.starts_with("**Agent:")
            || line.starts_with("**You:")
            || line.starts_with("**Tool:")
        {
            break;
        }
    }
    Err("Could not find tool name in previous lines".to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sven_model::{FunctionCall, Message, MessageContent, Role};

    use super::*;
    use crate::chat::segment::ChatSegment;

    fn user_seg(text: &str) -> ChatSegment {
        ChatSegment::Message(Message { role: Role::User, content: MessageContent::Text(text.into()) })
    }
    fn agent_seg(text: &str) -> ChatSegment {
        ChatSegment::Message(Message { role: Role::Assistant, content: MessageContent::Text(text.into()) })
    }
    fn tool_call_seg(call_id: &str, name: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: call_id.into(),
                function: FunctionCall { name: name.into(), arguments: "{}".into() },
            },
        })
    }
    fn tool_result_seg(call_id: &str, output: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: call_id.into(),
                content: output.into(),
            },
        })
    }

    // ── message_to_markdown ───────────────────────────────────────────────────

    #[test]
    fn user_message_formatted_with_separator_and_you_label() {
        let msg   = Message { role: Role::User, content: MessageContent::Text("hello world".into()) };
        let cache = HashMap::new();
        let md = message_to_markdown(&msg, &cache);
        assert!(md.starts_with("---"),       "must start with --- separator; got: {:?}", md);
        assert!(md.contains("**You:**"),     "must carry **You:** label");
        assert!(md.contains("hello world"), "must contain the user text");
        assert!(!md.starts_with('\n'),       "separator must be the first character");
    }

    #[test]
    fn agent_message_formatted_with_agent_label() {
        let msg   = Message { role: Role::Assistant, content: MessageContent::Text("response text".into()) };
        let cache = HashMap::new();
        let md = message_to_markdown(&msg, &cache);
        assert!(md.contains("**Agent:**"),    "must carry **Agent:** label");
        assert!(md.contains("response text"), "must contain the agent text");
    }

    #[test]
    fn tool_call_formatted_with_tool_call_heading_and_name_appears_once() {
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "id1".into(),
                function: FunctionCall { name: "read_file".into(), arguments: r#"{"path":"/tmp/x"}"#.into() },
            },
        };
        let cache = HashMap::new();
        let md = message_to_markdown(&msg, &cache);
        assert!(md.contains("Tool Call"),  "must carry 'Tool Call' heading");
        assert!(md.contains("read_file"), "must include the tool name");
        let name_count = md.matches("read_file").count();
        assert_eq!(name_count, 1, "tool name must appear exactly once; found {name_count} in: {md:?}");
    }

    #[test]
    fn tool_result_formatted_with_response_heading_output_and_name_appears_once() {
        let mut cache = HashMap::new();
        cache.insert("id1".to_string(), "read_file".to_string());
        let msg = Message {
            role: Role::Tool,
            content: MessageContent::ToolResult { tool_call_id: "id1".into(), content: "file contents here".into() },
        };
        let md = message_to_markdown(&msg, &cache);
        assert!(md.contains("Tool Response"),       "must carry 'Tool Response' heading");
        assert!(md.contains("file contents here"),  "must include the tool output");
        assert!(md.contains("```"),                 "output must be inside a code fence");
        let name_count = md.matches("read_file").count();
        assert_eq!(name_count, 1, "tool name must appear exactly once; found {name_count} in: {md:?}");
    }

    // ── format_conversation ───────────────────────────────────────────────────

    #[test]
    fn empty_conversation_produces_empty_output() {
        let result = format_conversation(&[], "", &HashMap::new());
        assert!(result.trim().is_empty(), "empty conversation must produce empty string; got: {result:?}");
    }

    #[test]
    fn single_user_message_starts_without_leading_newline() {
        let result = format_conversation(&[user_seg("hello")], "", &HashMap::new());
        assert!(result.starts_with("---"), "must begin with --- separator; got: {result:?}");
        assert!(!result.starts_with('\n'), "must not have a leading newline");
    }

    #[test]
    fn conversation_with_user_and_agent_contains_both_labels_and_texts() {
        let result = format_conversation(&[user_seg("question"), agent_seg("answer")], "", &HashMap::new());
        assert!(result.contains("**You:**"),   "You label present");
        assert!(result.contains("question"),   "user text present");
        assert!(result.contains("**Agent:**"), "Agent label present");
        assert!(result.contains("answer"),     "agent text present");
    }

    #[test]
    fn multi_turn_conversation_has_no_triple_newlines() {
        let segs = vec![user_seg("a"), agent_seg("b"), user_seg("c"), agent_seg("d")];
        let result = format_conversation(&segs, "", &HashMap::new());
        assert!(!result.contains("\n\n\n"), "triple newlines must not appear; got:\n{result}");
    }

    #[test]
    fn streaming_buffer_appended_after_all_committed_segments() {
        let result = format_conversation(&[user_seg("hello")], "partial response", &HashMap::new());
        assert!(result.contains("hello"),            "committed segment present");
        assert!(result.contains("partial response"), "streaming buffer present");
        let user_pos   = result.find("hello").unwrap();
        let stream_pos = result.find("partial response").unwrap();
        assert!(stream_pos > user_pos, "streaming text must come after the committed segment");
    }

    #[test]
    fn tool_call_and_result_both_appear_with_name_at_most_twice() {
        let mut cache = HashMap::new();
        cache.insert("id1".to_string(), "glob".to_string());
        let segs = vec![user_seg("find files"), tool_call_seg("id1", "glob"), tool_result_seg("id1", "result.txt")];
        let result = format_conversation(&segs, "", &cache);
        assert!(result.contains("glob"),       "tool name must appear");
        assert!(result.contains("result.txt"), "tool output must appear");
        let name_count = result.matches("glob").count();
        assert!(name_count <= 2, "tool name must appear at most twice; found {name_count}:\n{result}");
    }

    // ── parse_markdown_to_messages ────────────────────────────────────────────

    #[test]
    fn parse_empty_markdown_produces_empty_messages() {
        assert!(parse_markdown_to_messages("").unwrap().is_empty());
    }

    #[test]
    fn parse_single_user_message_extracts_role_and_text() {
        let messages = parse_markdown_to_messages("---\n\n**You:** hello world\n").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].as_text(), Some("hello world"));
    }

    #[test]
    fn parse_user_and_agent_messages_preserves_order_and_content() {
        let messages = parse_markdown_to_messages("---\n\n**You:** question\n\n**Agent:** answer\n").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].as_text(), Some("question"));
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].as_text(), Some("answer"));
    }

    #[test]
    fn parse_tool_call_extracts_id_name_and_full_args() {
        let md = concat!(
            "**Agent:tool_call:abc123**\n",
            "🔧 **Tool Call: read_file**\n",
            "```json\n",
            r#"{"path": "/tmp/test.txt"}"#, "\n",
            "```\n",
        );
        let messages = parse_markdown_to_messages(md).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        if let MessageContent::ToolCall { tool_call_id, function } = &messages[0].content {
            assert_eq!(tool_call_id, "abc123");
            assert_eq!(function.name, "read_file");
            assert_eq!(function.arguments.trim(), r#"{"path": "/tmp/test.txt"}"#);
        } else {
            panic!("expected ToolCall content");
        }
    }

    #[test]
    fn parse_tool_result_extracts_id_and_full_output() {
        let md = concat!(
            "**Tool:xyz789**\n",
            "✅ **Tool Response: glob**\n",
            "```\n",
            "file1.rs\n",
            "file2.rs\n",
            "```\n",
        );
        let messages = parse_markdown_to_messages(md).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Tool);
        if let MessageContent::ToolResult { tool_call_id, content } = &messages[0].content {
            assert_eq!(tool_call_id, "xyz789");
            assert_eq!(content.to_string().trim(), "file1.rs\nfile2.rs");
        } else {
            panic!("expected ToolResult content");
        }
    }

    #[test]
    fn parse_system_message_extracts_role_and_text() {
        let messages = parse_markdown_to_messages("**System:** You are a helpful assistant.\n\n").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].as_text(), Some("You are a helpful assistant."));
    }

    #[test]
    fn roundtrip_user_and_agent_messages_preserves_content() {
        let original = vec![
            Message::user("first question"),
            Message::assistant("first answer"),
            Message::user("second question"),
        ];
        let cache = HashMap::new();
        let md: String = original.iter().map(|m| message_to_markdown(m, &cache)).collect();
        let parsed = parse_markdown_to_messages(&md).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].as_text(), original[0].as_text());
        assert_eq!(parsed[1].as_text(), original[1].as_text());
        assert_eq!(parsed[2].as_text(), original[2].as_text());
    }

    #[test]
    fn roundtrip_conversation_with_tool_call_and_result_preserves_all_data() {
        let mut cache = HashMap::new();
        cache.insert("call1".to_string(), "read_file".to_string());
        let original = vec![
            Message::user("read the file"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call1".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"/tmp/x","encoding":"utf8"}"#.into(),
                    },
                },
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult {
                    tool_call_id: "call1".into(),
                    content: "file contents\nline two\nline three".into(),
                },
            },
            Message::assistant("The file contains three lines"),
        ];
        let md: String = original.iter().map(|m| message_to_markdown(m, &cache)).collect();
        let parsed = parse_markdown_to_messages(&md).unwrap();
        assert_eq!(parsed.len(), 4, "all messages must be preserved");
        assert_eq!(parsed[0].role, Role::User);
        assert_eq!(parsed[0].as_text(), Some("read the file"));
        if let MessageContent::ToolCall { tool_call_id, function } = &parsed[1].content {
            assert_eq!(tool_call_id, "call1");
            assert_eq!(function.name, "read_file");
            assert!(function.arguments.contains("utf8"), "full args must be preserved; got: {}", function.arguments);
        } else { panic!("expected ToolCall"); }
        if let MessageContent::ToolResult { tool_call_id, content } = &parsed[2].content {
            assert_eq!(tool_call_id, "call1");
            assert!(content.to_string().contains("line three"), "full output must be preserved; got: {}", content);
        } else { panic!("expected ToolResult"); }
        assert_eq!(parsed[3].as_text(), Some("The file contains three lines"));
    }

    #[test]
    fn parse_multiline_user_message_joins_lines() {
        let md = "**You:** first line\nsecond line\nthird line\n";
        let messages = parse_markdown_to_messages(md).unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0].as_text().unwrap();
        assert!(text.contains("first line"),  "first line present");
        assert!(text.contains("second line"), "second line present");
        assert!(text.contains("third line"),  "third line present");
    }

    #[test]
    fn parse_stops_at_next_message_header() {
        let md = "**You:** first\n\n**Agent:** second\n";
        let messages = parse_markdown_to_messages(md).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].as_text(), Some("first"));
        assert_eq!(messages[1].as_text(), Some("second"));
    }

    #[test]
    fn multi_paragraph_user_message_blank_lines_preserved_in_round_trip() {
        // A user message that contains a blank line (paragraph break) must
        // survive format → parse without losing the blank line.
        let msg = Message::user("First paragraph\n\nSecond paragraph");
        let cache = HashMap::new();
        let md = message_to_markdown(&msg, &cache);
        let messages = parse_markdown_to_messages(&md).unwrap();
        assert_eq!(messages.len(), 1, "should produce exactly one message; md was: {md:?}");
        let text = messages[0].as_text().unwrap();
        assert!(
            text.contains("First paragraph\n\nSecond paragraph"),
            "blank line between paragraphs must be preserved; got: {text:?}",
        );
    }

    #[test]
    fn parse_continuation_lines_with_bold_markdown_not_truncated() {
        // A continuation line that starts with ** (bold text) must NOT be
        // treated as a message header and must be preserved in the parsed text.
        let md = "**You:** first line\n**bold continuation**\nthird line\n";
        let messages = parse_markdown_to_messages(md).unwrap();
        assert_eq!(messages.len(), 1, "should produce exactly one message");
        let text = messages[0].as_text().unwrap();
        assert!(text.contains("bold continuation"),
            "bold continuation line must be preserved; got: {:?}", text);
        assert!(text.contains("third line"),
            "third line must be present; got: {:?}", text);
    }

    #[test]
    fn edit_first_user_message_then_parse_produces_correct_messages_for_resubmit() {
        let mut cache = HashMap::new();
        cache.insert("c1".to_string(), "glob".to_string());
        let original_segments = vec![
            user_seg("ORIGINAL_QUESTION"),
            agent_seg("first answer"),
            tool_call_seg("c1", "glob"),
            tool_result_seg("c1", "file.rs"),
            agent_seg("Found file"),
            user_seg("second question"),
            agent_seg("second answer"),
        ];
        let original_md = format_conversation(&original_segments, "", &cache);
        let edited_md = original_md.replace("ORIGINAL_QUESTION", "EDITED_QUESTION");
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        assert_eq!(parsed.len(), 7, "all messages must be present");
        assert_eq!(parsed[0].as_text(), Some("EDITED_QUESTION"), "first message was edited");
        assert_eq!(parsed[1].as_text(), Some("first answer"), "second message unchanged");
        assert_eq!(parsed[5].as_text(), Some("second question"), "later messages unchanged");
        if let MessageContent::ToolCall { tool_call_id, function } = &parsed[2].content {
            assert_eq!(tool_call_id, "c1");
            assert_eq!(function.name, "glob");
        } else { panic!("tool call structure must be preserved"); }
    }

    #[test]
    fn edit_middle_agent_response_then_parse_truncates_correctly() {
        let original_segments = vec![
            user_seg("question 1"),
            agent_seg("EDIT_THIS_RESPONSE"),
            user_seg("question 2"),
            agent_seg("answer 2"),
        ];
        let cache = HashMap::new();
        let original_md = format_conversation(&original_segments, "", &cache);
        let edited_md = original_md.replace("EDIT_THIS_RESPONSE", "EDITED_RESPONSE");
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].as_text(), Some("question 1"));
        assert_eq!(parsed[1].as_text(), Some("EDITED_RESPONSE"), "agent message edited");
        assert_eq!(parsed[2].as_text(), Some("question 2"));
        assert_eq!(parsed[3].as_text(), Some("answer 2"));
    }

    #[test]
    fn delete_last_turn_by_removing_markdown_then_parse_produces_truncated_list() {
        let original_segments = vec![
            user_seg("keep this"),
            agent_seg("keep this too"),
            user_seg("DELETE_ME"),
            agent_seg("delete this also"),
        ];
        let cache = HashMap::new();
        let original_md = format_conversation(&original_segments, "", &cache);
        let lines: Vec<&str> = original_md.lines().collect();
        let truncated_lines: Vec<&str> = lines
            .iter()
            .take_while(|line| !line.contains("DELETE_ME"))
            .copied()
            .collect();
        let edited_md = truncated_lines.join("\n");
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].as_text(), Some("keep this"));
        assert_eq!(parsed[1].as_text(), Some("keep this too"));
    }
}
