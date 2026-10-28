// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The model-free parts of keeping a conversation inside its window: the
//! emergency trim that is the last resort, the todo list every compaction
//! carries over, and content-aware truncation of oversized tool results.
//! The summary-based compaction is in `fork`.
use sven_model::{Message, Role};
use sven_tool_api::OutputCategory;
use sven_vocab::{TodoItem, TodoStatus};

/// The todo list as the compaction re-injects it, or `None` when it is empty.
/// Ids, statuses and contents are copied as they are.
#[must_use]
pub fn todo_block(todos: &[TodoItem]) -> Option<String> {
    if todos.is_empty() {
        return None;
    }
    let lines: Vec<String> = todos
        .iter()
        .map(|t| {
            let status = match t.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Completed => "completed",
                TodoStatus::Cancelled => "cancelled",
            };
            format!("- [{status}] {}: {}", t.id, t.content)
        })
        .collect();
    Some(format!(
        "## Todo list (kept verbatim across compaction)\n{}",
        lines.join("\n")
    ))
}

fn first_user_message(messages: &[Message]) -> Option<Message> {
    messages.iter().find(|m| m.role == Role::User).cloned()
}

/// Advance `start` (an index into `messages`, already at `len - keep`) past
/// any leading `ToolResult`/`ToolCall` messages so a preserved slice always
/// begins at a clean boundary - i.e. not inside a tool-call/tool-result
/// group. Sending a `ToolResult` without its preceding assistant `ToolCall`
/// (or vice versa) causes a 400 error from most provider APIs.
fn clean_boundary_start(messages: &[Message], keep_n: usize) -> usize {
    use sven_model::MessageContent;

    let keep = keep_n.min(messages.len());
    let mut start = messages.len().saturating_sub(keep);
    while start < messages.len() {
        match &messages[start].content {
            MessageContent::ToolResult { .. } | MessageContent::ToolCall { .. } => {
                start += 1;
            }
            _ => break,
        }
    }
    start
}

/// Emergency fallback compaction used when the session is too large to fit even
/// a compaction prompt within the context window.
///
/// Drops all but the last `keep_n` non-system messages and prepends a canned
/// notice followed by the first user message (the task), unless that message
/// is already in the preserved tail.  No model call is made - this is a purely
/// deterministic operation that always succeeds regardless of session size.
///
/// The preserved slice is adjusted forward until it starts at a clean message
/// boundary - i.e. not inside a tool-call/tool-result group.  This prevents
/// sending orphaned `ToolResult` messages to the API without a preceding
/// assistant `ToolCall`, which causes a 400 error.
pub fn emergency_compact(
    messages: &mut Vec<Message>,
    system_msg: Option<Message>,
    keep_n: usize,
    todos: &[TodoItem],
) -> usize {
    let before = messages.len();
    let non_system: Vec<Message> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .cloned()
        .collect();
    let start = clean_boundary_start(&non_system, keep_n);
    let preserved: Vec<Message> = non_system[start..].to_vec();
    let task = first_user_message(&non_system[..start]);
    let notice = Message::assistant(
        "[Context emergency-compacted: earlier history was dropped to prevent a \
         context-window overflow. The original task message is kept below. \
         Continue from the most recent messages and re-check any earlier \
         decision before relying on it.]",
    );
    let notice = match todo_block(todos) {
        Some(block) => Message::assistant(format!(
            "{}\n\n{block}",
            notice.as_text().unwrap_or_default()
        )),
        None => notice,
    };
    messages.clear();
    if let Some(sys) = system_msg {
        messages.push(sys);
    }
    messages.push(notice);
    messages.extend(task);
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
/// - [`OutputCategory::Bounded`]: cut the body like `HeadTail`, but keep the
///   tool's footer (the block after the last blank line) intact.
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
        OutputCategory::Bounded => bounded(content, cap_chars, omitted_bytes),
        OutputCategory::Generic => {
            // `cap_chars` is a byte budget derived from a token estimate, so it
            // lands wherever it lands; slicing there splits a multi-byte
            // character and panics. This runs on *every* tool result, so any
            // non-ASCII output over the budget took the session down.
            let cap_chars = content.floor_char_boundary(cap_chars);
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

/// Longest footer [`OutputCategory::Bounded`] protects; a longer final block
/// is treated as body so a tool cannot exempt unbounded text from the cap.
const MAX_FOOTER_BYTES: usize = 1024;

/// Cut the body of a self-bounded result to fit `cap_chars` and append its
/// footer unchanged.
fn bounded(content: &str, cap_chars: usize, omitted_bytes: usize) -> String {
    let split = content
        .rfind("\n\n")
        .map(|p| p + 2)
        .filter(|&at| content.len() - at <= MAX_FOOTER_BYTES);
    let Some(at) = split else {
        return head_tail_lines(
            content,
            cap_chars,
            60,
            40,
            &format!("[... {{lines}} lines / {omitted_bytes} bytes omitted ...]"),
        );
    };
    let (body, footer) = content.split_at(at);
    let body = body.trim_end_matches('\n');
    let budget = cap_chars.saturating_sub(footer.len() + 2);
    let cut = head_tail_lines(
        body,
        budget,
        60,
        40,
        &format!("[... {{lines}} lines / {omitted_bytes} bytes omitted ...]"),
    );
    format!("{cut}\n\n{footer}")
}

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
        // `str::lines()` strips `\r`, so for CRLF input `kept` is shorter than
        // `content` and this cut lands at an arbitrary byte rather than the
        // line boundary it looks like it lands on.
        return content[..content.floor_char_boundary(cap_chars)].to_string();
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
        // Same hazard as the line-oriented path: the head/tail accounting is
        // in bytes and `lines()` drops `\r`, so this offset need not sit on a
        // character boundary.
        return content[..content.floor_char_boundary(cap_chars)].to_string();
    }
    let notice = notice_template.replace("{lines}", &omitted.to_string());
    format!("{head}\n{notice}\n{tail}")
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod truncation_boundary_tests {
    use super::*;

    /// `smart_truncate` runs on every tool result, and its byte budget is
    /// derived from a token estimate, so the cut lands wherever it lands.
    /// Slicing a `&str` there splits a multi-byte character and panics —
    /// killing the session on any non-ASCII tool output over budget.
    ///
    /// Sweeps every budget across content built from 1-, 2-, 3- and 4-byte
    /// characters, in every output category, including CRLF (which `lines()`
    /// strips, so `kept` and `content` disagree on length).
    #[test]
    fn smart_truncate_never_splits_a_character() {
        let corpora = [
            "aé€😀\n".repeat(40),
            "AB\r\nCé\r\n".repeat(40),
            "😀".repeat(80),
            "line one\nline two é\nline three 😀\n".repeat(20),
        ];
        let categories = [
            OutputCategory::Generic,
            OutputCategory::HeadTail,
            OutputCategory::MatchList,
            OutputCategory::FileContent,
            OutputCategory::Bounded,
        ];
        for content in &corpora {
            for category in categories {
                for cap_tokens in 0..80 {
                    // Must not panic, and must stay valid UTF-8 by construction.
                    let out = smart_truncate(content, category, cap_tokens);
                    assert!(out.is_char_boundary(0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{Message, Role};

    fn make_history() -> Vec<Message> {
        vec![
            Message::system("You are a helpful assistant."),
            Message::user("What is Rust?"),
            Message::assistant("Rust is a systems programming language."),
            Message::user("Show me an example."),
            Message::assistant("fn main() { println!(\"Hello\"); }"),
        ]
    }

    fn recorded_run() -> Vec<Message> {
        vec![
            Message::system("You are Sven."),
            Message::user("Fix the flaky retry test in crates/net."),
            Message::assistant("I will read the test first."),
            Message::assistant("Decision: wait on the backoff signal."),
            Message::user("Continue."),
            Message::assistant("Tests now pass; clippy still has to run."),
        ]
    }

    fn todos() -> Vec<sven_vocab::TodoItem> {
        use sven_vocab::{TodoItem, TodoStatus};
        vec![
            TodoItem {
                id: "1".into(),
                content: "Read the retry test".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                id: "2".into(),
                content: "Replace the sleep with the backoff signal".into(),
                status: TodoStatus::InProgress,
            },
        ]
    }

    #[test]
    fn the_todo_list_survives_an_emergency_compaction_with_the_task() {
        let mut msgs = recorded_run();
        emergency_compact(&mut msgs, None, 2, &todos());
        let text: Vec<&str> = msgs.iter().filter_map(Message::as_text).collect();
        assert!(
            text.iter().any(|t| t.contains("Fix the flaky retry test")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|t| t.contains("[in_progress] 2: Replace the sleep with the backoff signal")),
            "{text:?}"
        );
    }

    #[test]
    fn emergency_compact_returns_original_count() {
        let mut msgs = make_history();
        let before = emergency_compact(&mut msgs, None, 2, &[]);
        assert_eq!(before, 5);
    }

    #[test]
    fn emergency_compact_keeps_at_most_keep_n_non_system_messages() {
        let mut msgs = make_history();
        // 4 non-system messages; keep 2
        emergency_compact(&mut msgs, None, 2, &[]);
        // notice + the kept task + 2 preserved = 4 non-system messages
        let non_sys: Vec<_> = msgs.iter().filter(|m| m.role != Role::System).collect();
        assert_eq!(
            non_sys.len(),
            4,
            "notice + task + 2 preserved messages expected"
        );
    }

    #[test]
    fn emergency_compact_preserves_most_recent_messages() {
        let mut msgs = vec![
            Message::user("old message"),
            Message::assistant("old reply"),
            Message::user("recent message"),
            Message::assistant("recent reply"),
        ];
        emergency_compact(&mut msgs, None, 2, &[]);
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
    fn emergency_compact_keeps_the_task_that_fell_out_of_the_tail() {
        let mut msgs = vec![
            Message::user("the task"),
            Message::assistant("old reply"),
            Message::user("later message"),
            Message::assistant("later reply"),
        ];
        emergency_compact(&mut msgs, None, 2, &[]);
        let text: Vec<&str> = msgs.iter().filter_map(Message::as_text).collect();
        assert_eq!(text.iter().filter(|t| **t == "the task").count(), 1);
        let task = text.iter().position(|t| *t == "the task").unwrap();
        let tail = text.iter().position(|t| *t == "later message").unwrap();
        assert!(
            task < tail,
            "the task precedes the preserved tail: {text:?}"
        );
        assert!(
            !text.iter().any(|t| t.contains("re-provide")),
            "nobody can answer in a headless run: {text:?}"
        );
    }

    #[test]
    fn emergency_compact_does_not_repeat_a_task_inside_the_tail() {
        let mut msgs = vec![
            Message::assistant("old"),
            Message::user("the task"),
            Message::assistant("reply"),
        ];
        emergency_compact(&mut msgs, None, 2, &[]);
        let tasks = msgs
            .iter()
            .filter(|m| m.as_text() == Some("the task"))
            .count();
        assert_eq!(tasks, 1, "{msgs:?}");
    }

    #[test]
    fn emergency_compact_with_system_message_puts_sys_first() {
        let mut msgs = make_history();
        let sys = Message::system("system content");
        emergency_compact(&mut msgs, Some(sys), 2, &[]);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].as_text(), Some("system content"));
    }

    #[test]
    fn emergency_compact_notice_contains_warning_text() {
        let mut msgs = make_history();
        emergency_compact(&mut msgs, None, 2, &[]);
        let notice_text = msgs[0].as_text().unwrap();
        assert!(
            notice_text.contains("emergency-compacted"),
            "notice must mention emergency compaction"
        );
    }

    // ── smart_truncate ────────────────────────────────────────────────────────

    /// A tool that bounds its own output ends it with a footer (handle, exit
    /// code). When the global cap still applies, the body is cut and the
    /// footer survives byte for byte.
    #[test]
    fn bounded_category_keeps_the_footer() {
        let footer = "[output truncated]\nLines: 900 total\nExit: 3 | Buffer: buf_0001";
        let body: String = (0..900).map(|i| format!("row {i}\n")).collect();
        let content = format!("{body}\n{footer}");
        let out = smart_truncate(&content, OutputCategory::Bounded, 100);
        assert!(out.ends_with(footer), "{out}");
        assert!(out.len() < content.len() / 4, "body was cut: {}", out.len());
        assert!(out.contains("row 0\n") && out.contains("row 899"));
    }

    #[test]
    fn bounded_category_under_cap_is_untouched() {
        let content = "ok\n\nExit: 0 | Duration: 0.1s";
        assert_eq!(
            smart_truncate(content, OutputCategory::Bounded, 100),
            content
        );
    }

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
        // line 100 is in the middle - neither in the first 60 nor the last 40
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
            "MatchList must NOT jump to the tail - that distinguishes it from HeadTail"
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
        // A single long line with no newlines - hard cut at cap_chars.
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
