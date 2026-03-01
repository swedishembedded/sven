// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::CompactionStrategy;
use sven_model::{Message, Role};
use sven_tools::OutputCategory;

// ─── Compaction prompts ───────────────────────────────────────────────────────

const SUMMARIZE_PROMPT: &str =
    "You are a context compaction assistant. Summarise the following conversation history \
     in a concise, information-dense way. Preserve all technical details, decisions, file \
     names, code snippets, and tool outputs that may be relevant to future work. \
     The summary will replace the original history to free up context space.";

const STRUCTURED_COMPACTION_PROMPT: &str = "\
You are a context compaction assistant for a software engineering agent. \
Produce a structured state checkpoint from the conversation history below. \
Use EXACTLY the following Markdown sections — do not add or remove sections. \
Be information-dense: preserve file paths, function names, error messages, \
code snippets, test names, and technical decisions verbatim where they matter.

## Active Task
Describe in 1-3 sentences what the agent is currently working on.

## Key Decisions & Rationale
List every significant technical decision made and why (bullet points). \
Include file or component names.

## Files & Artifacts
List every file that was read, modified, or created, with a brief note on what was done.

## Constraints & Requirements
List every requirement, constraint, or user preference that must be preserved.

## Pending Items
List every unfinished subtask or open question.

## Session Narrative
Write a dense technical summary (2-5 paragraphs) of what happened, \
capturing the essential flow of events, tool outputs, and reasoning. \
Focus on facts the agent will need to continue correctly.";

// ─── Public API ───────────────────────────────────────────────────────────────

/// Replace the conversation history with a single summarisation request using
/// the legacy narrative strategy.  Kept for backward compatibility and direct
/// use in tests; prefer [`compact_session_with_strategy`] for new callers.
pub fn compact_session(messages: &mut Vec<Message>, system_msg: Option<Message>) -> usize {
    compact_session_with_strategy(messages, system_msg, &CompactionStrategy::Narrative)
}

/// Strategy-aware compaction: restructures the message list so that the model
/// will produce a summary (or structured checkpoint) on the next turn.
///
/// The caller is responsible for actually invoking the model and rebuilding
/// the session from the resulting summary text.  This function only rewrites
/// the `messages` list to contain the compaction prompt.
pub fn compact_session_with_strategy(
    messages: &mut Vec<Message>,
    system_msg: Option<Message>,
    strategy: &CompactionStrategy,
) -> usize {
    let before = messages.len();
    let prompt = match strategy {
        CompactionStrategy::Structured => STRUCTURED_COMPACTION_PROMPT,
        CompactionStrategy::Narrative => SUMMARIZE_PROMPT,
    };
    let history_text = serialize_history(messages);
    let summary_request = Message::user(format!("{prompt}\n\n---\n\n{history_text}"));
    messages.clear();
    if let Some(sys) = system_msg {
        messages.push(sys);
    }
    messages.push(summary_request);
    before
}

/// Emergency fallback compaction used when the session is too large to fit even
/// a compaction prompt within the context window.
///
/// Drops all but the last `keep_n` non-system messages and prepends a canned
/// notice.  No model call is made — this is a purely deterministic operation
/// that always succeeds regardless of session size.
pub fn emergency_compact(
    messages: &mut Vec<Message>,
    system_msg: Option<Message>,
    keep_n: usize,
) -> usize {
    let before = messages.len();
    let non_system: Vec<Message> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .cloned()
        .collect();
    let keep = keep_n.min(non_system.len());
    let preserved: Vec<Message> = non_system[non_system.len() - keep..].to_vec();
    let notice = Message::assistant(
        "[Context emergency-compacted: earlier history was dropped to prevent a \
         context-window overflow. The agent may lack full context for earlier \
         decisions. Proceed carefully and ask the user to re-provide any missing \
         requirements if needed.]",
    );
    messages.clear();
    if let Some(sys) = system_msg {
        messages.push(sys);
    }
    messages.push(notice);
    messages.extend(preserved);
    before
}

/// Deterministic, content-aware tool-result truncation.
///
/// Returns `content` unchanged when it fits within `cap_tokens`.
/// Otherwise applies a category-specific extraction strategy that preserves
/// the most useful portion of the output.  Dispatching on [`OutputCategory`]
/// (not on tool names) keeps this function independent of the tools crate's
/// concrete tool list; each tool declares its own category.
///
/// - [`OutputCategory::HeadTail`]: keep the first 60 + last 40 lines so both
///   the command preamble and the final result are visible.
/// - [`OutputCategory::MatchList`]: keep leading matches (highest relevance
///   first); the tail is not preserved because later matches are less relevant.
/// - [`OutputCategory::FileContent`]: balanced head + tail with a separator,
///   preserving both the imports/declarations and the most recent changes.
/// - [`OutputCategory::Generic`]: hard-truncate at the nearest line boundary.
///
/// Every truncated result ends with an explicit notice so the model knows
/// that additional content exists and how to retrieve it.
pub fn smart_truncate(content: &str, category: OutputCategory, cap_tokens: usize) -> String {
    if cap_tokens == 0 {
        return content.to_string();
    }
    let cap_chars = cap_tokens * 4;
    if content.len() <= cap_chars {
        return content.to_string();
    }
    let omitted_bytes = content.len().saturating_sub(cap_chars);
    match category {
        OutputCategory::HeadTail => head_tail_lines(
            content,
            cap_chars,
            60,
            40,
            &format!("[... {{lines}} lines / {omitted_bytes} bytes omitted ...]"),
        ),
        OutputCategory::MatchList => head_lines(
            content,
            cap_chars,
            &format!(
                "[... {{lines}} more matches omitted ({omitted_bytes} bytes); \
                     use a more specific pattern to see them ...]"
            ),
        ),
        OutputCategory::FileContent => head_tail_lines(
            content,
            cap_chars,
            usize::MAX,
            usize::MAX,
            &format!(
                "[... {{lines}} lines omitted ({omitted_bytes} bytes); \
                     use read_file with offset/limit to see more ...]"
            ),
        ),
        OutputCategory::Generic => {
            let cut = content[..cap_chars]
                .rfind('\n')
                .map(|p| p + 1)
                .unwrap_or(cap_chars);
            format!(
                "{}\n[... {omitted_bytes} bytes omitted; \
                 content truncated to fit context budget ...]",
                &content[..cut]
            )
        }
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Serialise a message list into plain text for inclusion in a compaction prompt.
fn serialize_history(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| !matches!(m.role, Role::System))
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => "System",
            };
            let text = match &m.content {
                sven_model::MessageContent::Text(t) => t.clone(),
                sven_model::MessageContent::ContentParts(parts) => parts
                    .iter()
                    .map(|p| match p {
                        sven_model::ContentPart::Text { text } => text.clone(),
                        sven_model::ContentPart::Image { .. } => "[image]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                sven_model::MessageContent::ToolCall { function, .. } => {
                    format!("[tool_call: {}({})]", function.name, function.arguments)
                }
                sven_model::MessageContent::ToolResult { content, .. } => {
                    format!("[tool_result: {content}]")
                }
            };
            format!("{role}: {text}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Keep only the leading lines that fit within `cap_chars`.
fn head_lines(content: &str, cap_chars: usize, notice_template: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut kept = String::with_capacity(cap_chars);
    let mut kept_count = 0usize;
    for line in &lines {
        let needed = if kept.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if kept.len() + needed > cap_chars {
            break;
        }
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str(line);
        kept_count += 1;
    }
    let omitted = lines.len().saturating_sub(kept_count);
    if omitted == 0 {
        return content[..cap_chars.min(content.len())].to_string();
    }
    let notice = notice_template.replace("{lines}", &omitted.to_string());
    format!("{kept}\n{notice}")
}

/// Keep `max_head` leading lines and `max_tail` trailing lines, inserting a
/// notice between them.  Pass `usize::MAX` to split evenly by character budget.
fn head_tail_lines(
    content: &str,
    cap_chars: usize,
    max_head: usize,
    max_tail: usize,
    notice_template: &str,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let half_cap = cap_chars / 2;

    // Head
    let mut head = String::with_capacity(half_cap);
    let mut head_count = 0usize;
    for line in lines.iter().take(max_head) {
        let needed = if head.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if head.len() + needed > half_cap {
            break;
        }
        if !head.is_empty() {
            head.push('\n');
        }
        head.push_str(line);
        head_count += 1;
    }

    // Tail (collect from the end)
    let mut tail_lines: Vec<&str> = Vec::new();
    let mut tail_chars = 0usize;
    for line in lines.iter().rev().take(max_tail) {
        let needed = if tail_lines.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if tail_chars + needed > half_cap {
            break;
        }
        tail_chars += needed;
        tail_lines.push(line);
    }
    tail_lines.reverse();
    let tail_count = tail_lines.len();
    let tail = tail_lines.join("\n");

    let omitted = lines.len().saturating_sub(head_count + tail_count);
    if omitted == 0 {
        return content[..cap_chars.min(content.len())].to_string();
    }
    let notice = notice_template.replace("{lines}", &omitted.to_string());
    format!("{head}\n{notice}\n{tail}")
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{FunctionCall, Message, MessageContent, Role};

    fn make_history() -> Vec<Message> {
        vec![
            Message::system("You are a helpful assistant."),
            Message::user("What is Rust?"),
            Message::assistant("Rust is a systems programming language."),
            Message::user("Show me an example."),
            Message::assistant("fn main() { println!(\"Hello\"); }"),
        ]
    }

    // ── compact_session (legacy narrative) ────────────────────────────────────

    #[test]
    fn returns_original_message_count() {
        let mut msgs = make_history();
        let before = compact_session(&mut msgs, None);
        assert_eq!(before, 5);
    }

    #[test]
    fn output_has_single_user_summary_request_without_system() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn output_with_system_message_has_two_messages() {
        let mut msgs = make_history();
        let sys = Message::system("Keep this system message.");
        compact_session(&mut msgs, Some(sys));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].role, Role::User);
    }

    #[test]
    fn system_message_content_is_preserved() {
        let mut msgs = make_history();
        let sys = Message::system("Custom system prompt.");
        compact_session(&mut msgs, Some(sys));
        assert_eq!(msgs[0].as_text(), Some("Custom system prompt."));
    }

    #[test]
    fn summary_request_contains_original_text() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        let summary_text = msgs[0].as_text().unwrap();
        assert!(summary_text.contains("What is Rust?"));
        assert!(summary_text.contains("systems programming language"));
    }

    #[test]
    fn system_messages_excluded_from_history_text() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        let summary_text = msgs[0].as_text().unwrap();
        assert!(!summary_text.contains("You are a helpful assistant"));
    }

    #[test]
    fn tool_call_serialised_in_history() {
        let mut msgs = vec![
            Message::user("run ls"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "id1".into(),
                    function: FunctionCall {
                        name: "shell".into(),
                        arguments: r#"{"command":"ls"}"#.into(),
                    },
                },
            },
        ];
        compact_session(&mut msgs, None);
        let text = msgs[0].as_text().unwrap();
        assert!(text.contains("shell"), "tool name should appear in history");
        assert!(text.contains("ls"), "tool arg should appear in history");
    }

    #[test]
    fn tool_result_serialised_in_history() {
        let mut msgs = vec![
            Message::user("run ls"),
            Message::tool_result("id1", "file1.txt\nfile2.txt"),
        ];
        compact_session(&mut msgs, None);
        let text = msgs[0].as_text().unwrap();
        assert!(text.contains("file1.txt"));
    }

    #[test]
    fn compact_empty_history_returns_zero() {
        let mut msgs: Vec<Message> = vec![];
        let count = compact_session(&mut msgs, None);
        assert_eq!(count, 0);
    }

    #[test]
    fn compact_empty_history_produces_single_request() {
        let mut msgs: Vec<Message> = vec![];
        compact_session(&mut msgs, None);
        assert_eq!(msgs.len(), 1);
    }

    // ── compact_session_with_strategy (structured) ────────────────────────────

    #[test]
    fn structured_compaction_prompt_contains_required_sections() {
        let mut msgs = make_history();
        compact_session_with_strategy(&mut msgs, None, &CompactionStrategy::Structured);
        let text = msgs[0].as_text().unwrap();
        assert!(
            text.contains("## Active Task"),
            "missing Active Task section"
        );
        assert!(
            text.contains("## Key Decisions"),
            "missing Key Decisions section"
        );
        assert!(
            text.contains("## Files & Artifacts"),
            "missing Files section"
        );
        assert!(
            text.contains("## Constraints"),
            "missing Constraints section"
        );
        assert!(
            text.contains("## Pending Items"),
            "missing Pending Items section"
        );
        assert!(
            text.contains("## Session Narrative"),
            "missing Narrative section"
        );
    }

    #[test]
    fn structured_compaction_includes_history() {
        let mut msgs = make_history();
        compact_session_with_strategy(&mut msgs, None, &CompactionStrategy::Structured);
        let text = msgs[0].as_text().unwrap();
        assert!(
            text.contains("What is Rust?"),
            "history must be embedded in prompt"
        );
    }

    // ── emergency_compact ─────────────────────────────────────────────────────

    #[test]
    fn emergency_compact_returns_original_count() {
        let mut msgs = make_history();
        let before = emergency_compact(&mut msgs, None, 2);
        assert_eq!(before, 5);
    }

    #[test]
    fn emergency_compact_keeps_at_most_keep_n_non_system_messages() {
        let mut msgs = make_history();
        // 4 non-system messages; keep 2
        emergency_compact(&mut msgs, None, 2);
        // notice + 2 preserved = 3 non-system messages
        let non_sys: Vec<_> = msgs.iter().filter(|m| m.role != Role::System).collect();
        assert_eq!(non_sys.len(), 3, "notice + 2 preserved messages expected");
    }

    #[test]
    fn emergency_compact_preserves_most_recent_messages() {
        let mut msgs = vec![
            Message::user("old message"),
            Message::assistant("old reply"),
            Message::user("recent message"),
            Message::assistant("recent reply"),
        ];
        emergency_compact(&mut msgs, None, 2);
        let text: Vec<String> = msgs
            .iter()
            .filter_map(|m| m.as_text().map(|t| t.to_string()))
            .collect();
        assert!(
            text.iter().any(|t| t.contains("recent message")),
            "most recent user message must be preserved"
        );
        assert!(
            text.iter().any(|t| t.contains("recent reply")),
            "most recent assistant reply must be preserved"
        );
    }

    #[test]
    fn emergency_compact_with_system_message_puts_sys_first() {
        let mut msgs = make_history();
        let sys = Message::system("system content");
        emergency_compact(&mut msgs, Some(sys), 2);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].as_text(), Some("system content"));
    }

    #[test]
    fn emergency_compact_notice_contains_warning_text() {
        let mut msgs = make_history();
        emergency_compact(&mut msgs, None, 2);
        let notice_text = msgs[0].as_text().unwrap();
        assert!(
            notice_text.contains("emergency-compacted"),
            "notice must mention emergency compaction"
        );
    }

    // ── smart_truncate ────────────────────────────────────────────────────────

    /// Build a multi-line string of exactly `n` lines, each of the form "line N".
    fn make_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // -- pass-through (no truncation) --

    #[test]
    fn smart_truncate_returns_original_when_under_cap() {
        let short = "hello world";
        assert_eq!(smart_truncate(short, OutputCategory::HeadTail, 100), short);
    }

    #[test]
    fn smart_truncate_zero_cap_returns_original() {
        let content = "a".repeat(10_000);
        assert_eq!(
            smart_truncate(&content, OutputCategory::HeadTail, 0),
            content
        );
    }

    #[test]
    fn smart_truncate_empty_content_returns_empty() {
        assert_eq!(smart_truncate("", OutputCategory::Generic, 10), "");
    }

    #[test]
    fn smart_truncate_exactly_at_cap_not_truncated() {
        // cap_chars = 10 * 4 = 40 bytes; content is exactly 40 bytes
        let content = "a".repeat(40);
        let result = smart_truncate(&content, OutputCategory::Generic, 10);
        assert_eq!(
            result, content,
            "content at exact cap boundary must not be truncated"
        );
    }

    #[test]
    fn smart_truncate_one_byte_over_cap_is_truncated() {
        // cap_chars = 10 * 4 = 40 bytes; content is 41 bytes
        let content = "a".repeat(41);
        let result = smart_truncate(&content, OutputCategory::Generic, 10);
        assert_ne!(
            result, content,
            "content one byte over cap must be truncated"
        );
        assert!(result.contains("omitted"));
    }

    // -- all categories add an omission notice --

    #[test]
    fn all_categories_add_omission_notice_when_truncated() {
        let content = make_lines(1000);
        for category in [
            OutputCategory::HeadTail,
            OutputCategory::MatchList,
            OutputCategory::FileContent,
            OutputCategory::Generic,
        ] {
            let result = smart_truncate(&content, category, 10);
            assert!(
                result.contains("omitted"),
                "{category:?} truncation must include an omission notice"
            );
        }
    }

    // -- HeadTail: keeps first and last lines --

    #[test]
    fn headtail_preserves_first_lines() {
        // 200 lines; cap 50 tokens (200 chars). HeadTail keeps lines 0-59 + last 40.
        let content = make_lines(200);
        let result = smart_truncate(&content, OutputCategory::HeadTail, 50);
        assert!(
            result.contains("line 0"),
            "HeadTail must preserve the first line"
        );
        assert!(
            result.contains("line 1"),
            "HeadTail must preserve early lines"
        );
    }

    #[test]
    fn headtail_preserves_last_lines() {
        let content = make_lines(200);
        let result = smart_truncate(&content, OutputCategory::HeadTail, 50);
        assert!(
            result.contains("line 199"),
            "HeadTail must preserve the last line"
        );
        assert!(
            result.contains("line 198"),
            "HeadTail must preserve recent lines"
        );
    }

    #[test]
    fn headtail_drops_middle_lines() {
        // With 200 lines and a tight cap, middle lines (e.g. line 100) must be gone.
        let content = make_lines(200);
        let result = smart_truncate(&content, OutputCategory::HeadTail, 50);
        // line 100 is in the middle — neither in the first 60 nor the last 40
        assert!(
            !result.contains("line 100\n") && !result.contains("\nline 100"),
            "HeadTail must drop middle lines that exceed the cap"
        );
    }

    // -- MatchList: keeps only leading content --

    #[test]
    fn matchlist_keeps_leading_matches() {
        let content = (0..500)
            .map(|i| format!("match {i}: some content"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = smart_truncate(&content, OutputCategory::MatchList, 50);
        assert!(
            result.contains("match 0:"),
            "MatchList must keep the first match"
        );
    }

    #[test]
    fn matchlist_does_not_preserve_trailing_content() {
        // 500 matches; with a small cap the last match must be gone.
        let content = (0..500)
            .map(|i| format!("match {i}: some content"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = smart_truncate(&content, OutputCategory::MatchList, 50);
        assert!(
            !result.contains("match 499:"),
            "MatchList must NOT jump to the tail — that distinguishes it from HeadTail"
        );
    }

    // -- FileContent: symmetric head + tail --

    #[test]
    fn filecontent_preserves_first_and_last_lines() {
        let content = make_lines(1000);
        let result = smart_truncate(&content, OutputCategory::FileContent, 50);
        assert!(
            result.contains("line 0"),
            "FileContent must preserve the first line"
        );
        assert!(
            result.contains("line 999"),
            "FileContent must preserve the last line"
        );
    }

    #[test]
    fn filecontent_drops_middle_lines() {
        let content = make_lines(1000);
        let result = smart_truncate(&content, OutputCategory::FileContent, 50);
        // With 1000 lines and a 200-char cap there is no room for line 500
        assert!(
            !result.contains("line 500\n") && !result.contains("\nline 500"),
            "FileContent must drop middle content"
        );
    }

    // -- Generic: hard-truncates at nearest newline --

    #[test]
    fn generic_truncates_at_newline_boundary() {
        // Build a string where the newline is well within the cap window.
        // cap = 5 tokens → 20 chars; content has a newline at position 10.
        let content = format!("{}\n{}", "a".repeat(10), "b".repeat(100));
        let result = smart_truncate(&content, OutputCategory::Generic, 5);
        // The cut should happen at the newline (position 11), not mid-word.
        assert!(
            !result.contains("bbb"),
            "Generic must not include content past the nearest newline"
        );
    }

    #[test]
    fn generic_falls_back_to_hard_cut_when_no_newline() {
        // A single long line with no newlines — hard cut at cap_chars.
        let content = "x".repeat(10_000);
        let result = smart_truncate(&content, OutputCategory::Generic, 10);
        // cap_chars = 40; result must be ≤ 40 chars of 'x' plus the notice
        let x_count = result.chars().take_while(|&c| c == 'x').count();
        assert_eq!(
            x_count, 40,
            "Generic must hard-cut at cap_chars when no newline is found"
        );
    }

    // -- Omission notice content --

    #[test]
    fn headtail_omission_notice_mentions_lines_and_bytes() {
        let content = make_lines(200);
        let result = smart_truncate(&content, OutputCategory::HeadTail, 20);
        assert!(
            result.contains("omitted"),
            "HeadTail notice must mention 'omitted'"
        );
        assert!(
            result.contains("bytes"),
            "HeadTail notice must state byte count"
        );
    }

    #[test]
    fn matchlist_omission_notice_mentions_matches() {
        let content = (0..500)
            .map(|i| format!("match {i}: foo"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = smart_truncate(&content, OutputCategory::MatchList, 20);
        assert!(
            result.contains("matches omitted"),
            "MatchList notice must mention 'matches omitted'"
        );
    }

    #[test]
    fn filecontent_omission_notice_suggests_offset_limit() {
        let content = make_lines(1000);
        let result = smart_truncate(&content, OutputCategory::FileContent, 20);
        assert!(
            result.contains("offset") || result.contains("limit"),
            "FileContent notice must suggest offset/limit to retrieve more"
        );
    }

    // -- legacy omission notice tests (kept for regression) --

    #[test]
    fn smart_truncate_shell_includes_omission_notice() {
        let content = make_lines(200);
        let result = smart_truncate(&content, OutputCategory::HeadTail, 50);
        assert!(
            result.contains("omitted"),
            "truncated HeadTail output must contain omission notice"
        );
    }

    #[test]
    fn smart_truncate_grep_includes_omission_notice() {
        let content = (0..500)
            .map(|i| format!("match {i}: some content here"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = smart_truncate(&content, OutputCategory::MatchList, 100);
        assert!(
            result.contains("matches omitted") || result.contains("omitted"),
            "truncated MatchList output must note omission"
        );
    }

    #[test]
    fn smart_truncate_read_file_includes_omission_notice() {
        let content = (0..500)
            .map(|i| format!("{i}: some source code line here"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = smart_truncate(&content, OutputCategory::FileContent, 100);
        assert!(
            result.contains("omitted"),
            "truncated FileContent output must contain omission notice"
        );
    }

    #[test]
    fn smart_truncate_respects_cap_approximately() {
        let content = "x".repeat(80_000); // 20000 tokens
        let result = smart_truncate(&content, OutputCategory::Generic, 100);
        // cap_chars = 400; result should be cap + notice, well under 1000
        assert!(
            result.len() < 1000,
            "truncated output should be close to cap size"
        );
    }
}
