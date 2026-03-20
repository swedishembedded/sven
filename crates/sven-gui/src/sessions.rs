// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session persistence helpers: markdown↔ChatDocument conversion plus disk I/O.

use sven_frontend::{
    markdown::{parse_markdown_blocks, MarkdownBlock},
    tool_view::extract_tool_view,
};
use sven_input::{
    chat_path, ensure_chat_dir, json_str_to_yaml, yaml_to_json_str, ChatDocument, ChatStatus,
    ChatUsage, SessionId, TurnRecord,
};

use crate::{
    highlight::highlight_code,
    plain_msg::{PlainChatMessage, PlainMdBlock, PlainTextRun},
};

// ── Inline markdown parsing ───────────────────────────────────────────────────

/// Parse a paragraph string into inline `PlainTextRun` spans.
/// Handles bold (`**`), italic (`*`/`_`), inline code (`` ` ``), and links
/// `[label](url)`.  Falls back to a single plain run when no markers are found.
pub fn parse_inline_runs(text: &str) -> Vec<PlainTextRun> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    // Wrap in a paragraph so pulldown-cmark emits inline events
    let wrapped = format!("{}\n", text);
    let parser = Parser::new_ext(&wrapped, opts);

    let mut runs: Vec<PlainTextRun> = Vec::new();
    let mut bold = false;
    let mut italic = false;
    let mut link_url = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_url = dest_url.to_string();
            }
            Event::End(TagEnd::Link) => {
                link_url.clear();
            }
            // Inline code spans are emitted as Event::Code, not Start/End(Tag::Code)
            Event::Code(t) => {
                runs.push(PlainTextRun {
                    text: t.to_string(),
                    is_code: true,
                    bold,
                    italic,
                    is_link: !link_url.is_empty(),
                    url: link_url.clone(),
                });
            }
            Event::Text(t) => {
                let s = t.to_string();
                if !s.is_empty() {
                    runs.push(PlainTextRun {
                        text: s,
                        bold,
                        italic,
                        is_code: false,
                        is_link: !link_url.is_empty(),
                        url: link_url.clone(),
                    });
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                runs.push(PlainTextRun::plain(" "));
            }
            _ => {}
        }
    }

    if runs.is_empty() {
        vec![PlainTextRun::plain(text)]
    } else {
        runs
    }
}

// ── Rich line splitting ────────────────────────────────────────────────────────

/// Split a flat list of inline runs into visual lines of at most `max_chars`
/// characters each.  Long runs are split at word boundaries where possible.
///
/// This allows Slint to render each line as a `HorizontalLayout` without
/// overflow, giving full inline formatting support regardless of text length.
pub fn split_runs_into_rich_lines(
    runs: Vec<PlainTextRun>,
    max_chars: usize,
) -> Vec<Vec<PlainTextRun>> {
    let mut lines: Vec<Vec<PlainTextRun>> = Vec::new();
    let mut current_line: Vec<PlainTextRun> = Vec::new();
    let mut current_len: usize = 0;

    for run in runs {
        // Handle explicit newlines by splitting the run text on '\n'
        let sub_texts: Vec<&str> = run.text.split('\n').collect();
        for (si, sub) in sub_texts.iter().enumerate() {
            if si > 0 {
                // Newline: flush current line
                if !current_line.is_empty() {
                    lines.push(std::mem::take(&mut current_line));
                } else {
                    lines.push(vec![]);
                }
                current_len = 0;
            }
            if sub.is_empty() {
                continue;
            }
            let run_len = sub.chars().count();

            if current_len == 0 || current_len + run_len <= max_chars {
                // Fits on current line
                current_line.push(PlainTextRun {
                    text: sub.to_string(),
                    bold: run.bold,
                    italic: run.italic,
                    is_code: run.is_code,
                    is_link: run.is_link,
                    url: run.url.clone(),
                });
                current_len += run_len;
            } else if run_len > max_chars {
                // Run is very long — split at word boundaries, filling lines
                let mut remaining: String = sub.to_string();
                while !remaining.is_empty() {
                    let available = max_chars.saturating_sub(current_len);
                    let take = if available == 0 {
                        // Flush current line and start fresh
                        if !current_line.is_empty() {
                            lines.push(std::mem::take(&mut current_line));
                            current_len = 0;
                        }
                        max_chars
                    } else {
                        available
                    };

                    // Find a good split point (word boundary within `take` chars)
                    let chars: Vec<char> = remaining.chars().collect();
                    let split_at = if chars.len() <= take {
                        chars.len()
                    } else {
                        // Walk back from `take` to find a space
                        let mut bp = take;
                        while bp > 0 && chars[bp - 1] != ' ' {
                            bp -= 1;
                        }
                        if bp == 0 {
                            take
                        } else {
                            bp
                        }
                    };

                    let piece: String = chars[..split_at].iter().collect();
                    let piece_len = split_at;
                    current_line.push(PlainTextRun {
                        text: piece,
                        bold: run.bold,
                        italic: run.italic,
                        is_code: run.is_code,
                        is_link: run.is_link,
                        url: run.url.clone(),
                    });
                    current_len += piece_len;
                    remaining = chars[split_at..].iter().collect();

                    if current_len >= max_chars && !remaining.is_empty() {
                        lines.push(std::mem::take(&mut current_line));
                        current_len = 0;
                    }
                }
            } else {
                // Doesn't fit — flush current line, start new one
                if !current_line.is_empty() {
                    lines.push(std::mem::take(&mut current_line));
                    current_len = 0;
                }
                current_line.push(PlainTextRun {
                    text: sub.to_string(),
                    bold: run.bold,
                    italic: run.italic,
                    is_code: run.is_code,
                    is_link: run.is_link,
                    url: run.url.clone(),
                });
                current_len += run_len;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        lines.push(vec![]);
    }

    lines
}

// ── Markdown → PlainChatMessage ───────────────────────────────────────────────

/// Format tool fields as a readable multi-line string.
pub fn format_fields_json(fields: &[(String, String)]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|(k, v)| {
            let v_short: String = v.chars().take(120).collect();
            let v_display = if v.len() > 120 {
                format!("{v_short}…")
            } else {
                v_short
            };
            format!("{k}: {v_display}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Convert markdown text into a flat list of `PlainMdBlock`s suitable for use
/// as sub-blocks inside `ThinkingBubble` or `ToolCallBubble`.
///
/// Applies the same pipeline as `markdown_to_plain_messages`: block parsing,
/// inline run extraction, line wrapping, and syntax highlighting for code.
pub fn markdown_to_md_blocks(text: &str) -> Vec<PlainMdBlock> {
    let blocks = parse_markdown_blocks(text);
    let mut result = Vec::with_capacity(blocks.len());

    for block in blocks {
        let mb = match block {
            MarkdownBlock::Paragraph(text) => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs, 80);
                PlainMdBlock {
                    kind: "paragraph",
                    content: text,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::Heading { level, text } => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs, 80);
                PlainMdBlock {
                    kind: "heading",
                    content: text,
                    heading_level: level as i32,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::CodeBlock { language, code } => {
                let code_lines = highlight_code(&language, &code);
                PlainMdBlock {
                    kind: "code-block",
                    content: code,
                    language,
                    code_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::ListItem {
                depth,
                text,
                ordered,
                task_checked: _,
            } => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs, 72);
                PlainMdBlock {
                    kind: "list-item",
                    content: text,
                    heading_level: depth as i32,
                    is_ordered: ordered,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::BlockQuote(text) => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs, 76);
                PlainMdBlock {
                    kind: "block-quote",
                    content: text,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::Separator => PlainMdBlock {
                kind: "separator",
                ..Default::default()
            },
            MarkdownBlock::InlineCode(text) => PlainMdBlock {
                kind: "inline-code",
                content: text,
                ..Default::default()
            },
            MarkdownBlock::TableRow(cells) => {
                let content = cells.join(" │ ");
                PlainMdBlock {
                    kind: "table-row",
                    content,
                    cells,
                    ..Default::default()
                }
            }
        };
        result.push(mb);
    }

    result
}

/// Todo status icons (○ pending, ✓ completed, → in_progress, ✗ cancelled).
const TODO_ICONS: &[char] = &['○', '✓', '→', '✗'];

/// Parse todo tool output ("○ [1] Task\n✓ [2] Done") into todo-item blocks.
fn parse_todo_result(result: &str) -> Vec<PlainMdBlock> {
    result
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (icon, content) = match line.chars().next() {
                Some(first) if TODO_ICONS.contains(&first) => (
                    first.to_string(),
                    line[first.len_utf8()..].trim_start().to_string(),
                ),
                _ => ("○".to_string(), line.to_string()),
            };
            Some(PlainMdBlock {
                kind: "todo-item",
                content,
                icon,
                ..Default::default()
            })
        })
        .collect()
}

/// Parse tool result into blocks. For todo tool, uses todo-item formatting.
pub fn build_tool_result_blocks(result: &str, tool_name: Option<&str>) -> Vec<PlainMdBlock> {
    if tool_name == Some("todo") {
        let blocks = parse_todo_result(result);
        if blocks.is_empty() {
            vec![PlainMdBlock {
                kind: "paragraph",
                content: result.to_string(),
                ..Default::default()
            }]
        } else {
            blocks
        }
    } else {
        let blocks = markdown_to_md_blocks(result);
        if blocks.is_empty() {
            vec![PlainMdBlock {
                kind: "paragraph",
                content: result.to_string(),
                ..Default::default()
            }]
        } else {
            blocks
        }
    }
}

/// Convert markdown text into a sequence of `PlainChatMessage`s (one per block).
/// Code blocks are syntax-highlighted; assistant paragraphs get inline run parsing.
/// When `role == "user"`, the first block uses message_type "user" for UserBubble rendering.
pub fn markdown_to_plain_messages(text: &str, role: &'static str) -> Vec<PlainChatMessage> {
    let blocks = parse_markdown_blocks(text);
    if blocks.is_empty() {
        let msg_type = if role == "user" { "user" } else { "assistant" };
        return vec![PlainChatMessage {
            message_type: msg_type,
            content: text.to_string(),
            role,
            is_first_in_group: true,
            text_runs: vec![PlainTextRun::plain(text)],
            ..Default::default()
        }];
    }

    let mut messages: Vec<PlainChatMessage> = Vec::with_capacity(blocks.len());
    let mut is_first = true;

    for block in blocks {
        let msg = match block {
            MarkdownBlock::Paragraph(text) => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs.clone(), 80);
                let msg_type = if role == "user" && is_first {
                    "user"
                } else {
                    "assistant"
                };
                PlainChatMessage {
                    message_type: msg_type,
                    content: text,
                    role,
                    is_first_in_group: is_first,
                    text_runs: runs,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::Heading { level, text } => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs.clone(), 80);
                PlainChatMessage {
                    message_type: "heading",
                    content: text,
                    role,
                    is_first_in_group: is_first,
                    heading_level: level as i32,
                    text_runs: runs,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::CodeBlock { language, code } => {
                let code_lines = highlight_code(&language, &code);
                PlainChatMessage {
                    message_type: "code-block",
                    content: code,
                    role,
                    is_first_in_group: is_first,
                    language,
                    code_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::ListItem {
                depth,
                text,
                ordered,
                task_checked: _,
            } => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs.clone(), 72);
                PlainChatMessage {
                    message_type: "list-item",
                    content: text,
                    role,
                    is_first_in_group: is_first,
                    heading_level: depth as i32,
                    is_ordered_list: ordered,
                    text_runs: runs,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::Separator => PlainChatMessage {
                message_type: "separator",
                content: String::new(),
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::BlockQuote(text) => {
                let runs = parse_inline_runs(&text);
                let rich_lines = split_runs_into_rich_lines(runs.clone(), 76);
                PlainChatMessage {
                    message_type: "block-quote",
                    content: text,
                    role,
                    is_first_in_group: is_first,
                    text_runs: runs,
                    rich_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::InlineCode(text) => PlainChatMessage {
                message_type: "inline-code",
                content: text,
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::TableRow(cells) => {
                let content = cells.join(" │ ");
                PlainChatMessage {
                    message_type: "table-row",
                    content,
                    role,
                    is_first_in_group: is_first,
                    cells,
                    ..Default::default()
                }
            }
        };
        is_first = false;
        messages.push(msg);
    }

    messages
}

/// Strip common inline markdown markers for live-streaming preview.
/// Removes bold/italic/code markers so partial streaming text looks clean.
/// Handles incomplete code blocks (``` without closing) by stripping the fence.
pub fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_block = false;
    let mut code_block_content = String::new();

    for line in text.lines() {
        let trimmed = line.trim_start_matches('#').trim_start();

        // Detect code block fences
        if trimmed.starts_with("```") {
            if in_code_block {
                // Closing fence - emit accumulated code
                out.push_str(&code_block_content);
                code_block_content.clear();
                in_code_block = false;
            } else {
                // Opening fence - start accumulating
                in_code_block = true;
            }
            out.push('\n');
            continue;
        }

        if in_code_block {
            code_block_content.push_str(line);
            code_block_content.push('\n');
            continue;
        }

        // Strip inline formatting
        let cleaned = trimmed
            .replace("**", "")
            .replace("__", "")
            .replace("~~", "")
            .replace("* ", " ")
            .replace(" *", " ")
            .replace("_ ", " ")
            .replace(" _", " ");
        // Strip inline code backticks (single pairs)
        let cleaned = strip_inline_code_backticks(&cleaned);
        out.push_str(&cleaned);
        out.push('\n');
    }

    if in_code_block {
        out.push_str(&code_block_content);
    }
    out.trim_end().to_string()
}

/// Remove `inline code` backticks, leaving the inner text.
fn strip_inline_code_backticks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            // Skip until next backtick or end
            while let Some(n) = chars.next() {
                if n == '`' {
                    break;
                }
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Session persistence ───────────────────────────────────────────────────────

/// Convert a `ChatDocument`'s turns to `PlainChatMessage`s for display.
pub fn chat_document_to_plain_messages(doc: &ChatDocument) -> Vec<PlainChatMessage> {
    let mut out = Vec::new();
    for turn in &doc.turns {
        match turn {
            TurnRecord::User { content } => {
                out.extend(markdown_to_plain_messages(content, "user"));
            }
            TurnRecord::Assistant { content } => {
                out.extend(markdown_to_plain_messages(content, "assistant"));
            }
            TurnRecord::Thinking { content } => {
                let sub_blocks = markdown_to_md_blocks(content);
                let preview = content
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string();
                out.push(PlainChatMessage {
                    message_type: "thinking",
                    content: content.clone(),
                    thinking_preview: preview,
                    role: "thinking",
                    is_first_in_group: false,
                    sub_blocks,
                    ..Default::default()
                });
            }
            TurnRecord::ToolCall {
                tool_call_id: _,
                name,
                arguments,
            } => {
                let args_json = yaml_to_json_str(arguments);
                let args_value: serde_json::Value = serde_json::from_str(&args_json)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let view = extract_tool_view(name, &args_value, None);
                let fields_json = format_fields_json(&view.fields);
                let is_expanded = name == "todo";
                out.push(PlainChatMessage {
                    message_type: "tool-call",
                    content: args_json,
                    role: "assistant",
                    tool_name: name.clone(),
                    tool_icon: view.icon,
                    tool_summary: view.summary,
                    tool_category: view.category,
                    tool_fields_json: fields_json,
                    is_expanded,
                    ..Default::default()
                });
            }
            TurnRecord::ToolResult {
                tool_call_id: _,
                content,
            } => {
                let preview: String = content.chars().take(500).collect();
                let tool_name = out
                    .iter()
                    .rev()
                    .find(|m| m.message_type == "tool-call")
                    .map(|m| m.tool_name.as_str());
                let result_blocks = build_tool_result_blocks(&preview, tool_name);
                // Attach result to the preceding tool-call message
                if let Some(last) = out.iter_mut().rev().find(|m| m.message_type == "tool-call") {
                    last.tool_result_content = preview;
                    last.tool_result_blocks = result_blocks;
                }
            }
            TurnRecord::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                ..
            } => {
                let strat = strategy.as_deref().unwrap_or("unknown");
                out.push(PlainChatMessage::system(format!(
                    "Context compacted ({strat}): {tokens_before}→{tokens_after} tokens"
                )));
            }
        }
    }
    out
}

/// Convert a single block to its markdown representation.
pub fn block_to_markdown(p: &PlainChatMessage) -> String {
    match p.message_type {
        "user" | "assistant" => p.content.clone(),
        "code-block" => {
            let mut s = String::new();
            if !p.language.is_empty() {
                s.push_str("```");
                s.push_str(&p.language);
                s.push('\n');
            } else {
                s.push_str("```\n");
            }
            s.push_str(&p.content);
            s.push_str("\n```");
            s
        }
        "heading" => {
            let n = p.heading_level.max(1).min(6) as usize;
            format!("{} {}", "#".repeat(n), p.content)
        }
        "list-item" => {
            format!(
                "{}* {}",
                "  ".repeat(p.heading_level.max(0) as usize),
                p.content
            )
        }
        "block-quote" => format!("> {}", p.content.replace('\n', "\n> ")),
        "separator" => "---".to_string(),
        "inline-code" => format!("`{}`", p.content),
        "table-row" => p.cells.join(" | "),
        _ => p.content.clone(),
    }
}

/// Convert user blocks back to markdown for saving.
pub fn user_blocks_to_markdown(blocks: &[PlainChatMessage]) -> String {
    blocks
        .iter()
        .map(block_to_markdown)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Convert `PlainChatMessage` slice to `TurnRecord`s for saving to a `ChatDocument`.
pub fn plain_messages_to_turns(plain: &[PlainChatMessage]) -> Vec<TurnRecord> {
    let mut turns = Vec::new();
    let mut assistant_buf = String::new();
    let mut user_blocks: Vec<PlainChatMessage> = Vec::new();
    let mut tool_call_counter = 0u32;
    let mut last_tool_call_id: Option<String> = None;

    let flush_user = |turns: &mut Vec<TurnRecord>, blocks: &mut Vec<PlainChatMessage>| {
        if !blocks.is_empty() {
            turns.push(TurnRecord::User {
                content: user_blocks_to_markdown(blocks),
            });
            blocks.clear();
        }
    };

    for p in plain {
        let is_user_block = p.role == "user";

        match p.message_type {
            "user" if is_user_block => {
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                flush_user(&mut turns, &mut user_blocks);
                user_blocks.push(p.clone());
            }
            "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row"
                if is_user_block =>
            {
                user_blocks.push(p.clone());
            }
            "assistant" | "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row" => {
                flush_user(&mut turns, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    assistant_buf.push_str("\n\n");
                }
                assistant_buf.push_str(&block_to_markdown(p));
            }
            "tool-call" => {
                flush_user(&mut turns, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                let id = format!("call_{}", tool_call_counter);
                tool_call_counter += 1;
                last_tool_call_id = Some(id.clone());
                let arguments = json_str_to_yaml(&p.content);
                turns.push(TurnRecord::ToolCall {
                    tool_call_id: id.clone(),
                    name: p.tool_name.clone(),
                    arguments,
                });
                // Emit the merged tool result if present
                if !p.tool_result_content.is_empty() {
                    turns.push(TurnRecord::ToolResult {
                        tool_call_id: id,
                        content: p.tool_result_content.clone(),
                    });
                    last_tool_call_id = None;
                }
            }
            "tool-result" => {
                // Legacy standalone tool-result (from old sessions without merged results)
                if let Some(id) = last_tool_call_id.take() {
                    turns.push(TurnRecord::ToolResult {
                        tool_call_id: id,
                        content: p.content.clone(),
                    });
                }
            }
            "thinking" => {
                flush_user(&mut turns, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                turns.push(TurnRecord::Thinking {
                    content: p.content.clone(),
                });
            }
            "system" => {
                flush_user(&mut turns, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                if let Some((tb, ta, strat)) = parse_context_compacted(&p.content) {
                    turns.push(TurnRecord::ContextCompacted {
                        tokens_before: tb,
                        tokens_after: ta,
                        strategy: Some(strat),
                        turn: None,
                    });
                }
            }
            _ => {
                flush_user(&mut turns, &mut user_blocks);
            }
        }
    }
    flush_user(&mut turns, &mut user_blocks);
    if !assistant_buf.is_empty() {
        turns.push(TurnRecord::Assistant {
            content: assistant_buf,
        });
    }
    turns
}

/// Parse "Context compacted (X): N→M tokens" → `(before, after, strategy)`.
pub fn parse_context_compacted(s: &str) -> Option<(usize, usize, String)> {
    let rest = s.strip_prefix("Context compacted (")?;
    let (strat, rest) = rest.split_once("): ")?;
    let (before_str, after_str) = rest.split_once('→')?;
    let before = before_str.trim().parse().ok()?;
    let after = after_str.trim().trim_end_matches(" tokens").parse().ok()?;
    Some((before, after, strat.to_string()))
}

/// Persist a session's messages to disk in the same format used by the TUI.
pub fn save_session_to_disk(
    session_id: &str,
    plain: &[PlainChatMessage],
    title: &str,
    model: Option<&str>,
    mode: Option<&str>,
    usage: Option<ChatUsage>,
) {
    let turns = plain_messages_to_turns(plain);
    if turns.is_empty() {
        return;
    }
    let sid = SessionId::from_string(session_id.to_string());
    let path = chat_path(&sid);
    if let Err(e) = ensure_chat_dir() {
        tracing::warn!("cannot create chat dir: {e}");
        return;
    }
    let persisted_usage = usage.filter(|u| !u.is_empty());
    let mut doc = ChatDocument {
        id: sid,
        title: title.to_string(),
        model: model.map(String::from),
        mode: mode.map(String::from),
        status: ChatStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        parent_id: None,
        usage: persisted_usage,
        turns,
    };
    if let Err(e) = sven_input::save_chat_to(&path, &mut doc) {
        tracing::warn!("failed to save chat {}: {e}", path.display());
    }
}

/// Delete a session from disk.
pub fn delete_session_from_disk(session_id: &str) {
    let sid = SessionId::from_string(session_id.to_string());
    let path = chat_path(&sid);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("failed to delete chat {}: {e}", path.display());
        }
    }
}
