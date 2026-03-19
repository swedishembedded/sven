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
    plain_msg::{PlainChatMessage, PlainTextRun},
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
                    ..Default::default()
                }
            }
            MarkdownBlock::Heading { level, text } => PlainChatMessage {
                message_type: "heading",
                content: text,
                role,
                is_first_in_group: is_first,
                heading_level: level as i32,
                ..Default::default()
            },
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
            MarkdownBlock::ListItem { depth, text } => {
                let runs = parse_inline_runs(&text);
                PlainChatMessage {
                    message_type: "list-item",
                    content: text,
                    role,
                    is_first_in_group: is_first,
                    heading_level: depth as i32,
                    text_runs: runs,
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
            MarkdownBlock::BlockQuote(text) => PlainChatMessage {
                message_type: "block-quote",
                content: text,
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
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
/// Removes bold/italic markers so partial streaming text looks clean.
pub fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start_matches('#').trim_start();
        let cleaned = trimmed
            .replace("**", "")
            .replace("__", "")
            .replace("~~", "");
        let cleaned = cleaned
            .replace(" *", " ")
            .replace("* ", " ")
            .replace(" _", " ")
            .replace("_ ", " ");
        out.push_str(&cleaned);
        out.push('\n');
    }
    out.trim_end().to_string()
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
                out.push(PlainChatMessage {
                    message_type: "thinking",
                    content: content.clone(),
                    role: "thinking",
                    is_first_in_group: false,
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
                out.push(PlainChatMessage {
                    message_type: "tool-call",
                    content: args_json,
                    role: "assistant",
                    tool_name: name.clone(),
                    tool_icon: view.icon,
                    tool_summary: view.summary,
                    tool_category: view.category,
                    tool_fields_json: fields_json,
                    is_expanded: false,
                    ..Default::default()
                });
            }
            TurnRecord::ToolResult {
                tool_call_id: _,
                content,
            } => {
                let preview: String = content.chars().take(500).collect();
                out.push(PlainChatMessage {
                    message_type: "tool-result",
                    content: preview,
                    role: "tool",
                    ..Default::default()
                });
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

/// Convert user blocks back to markdown for saving.
fn user_blocks_to_markdown(blocks: &[PlainChatMessage]) -> String {
    let mut out = String::new();
    for (i, p) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        match p.message_type {
            "user" | "assistant" => out.push_str(&p.content),
            "code-block" => {
                if !p.language.is_empty() {
                    out.push_str("```");
                    out.push_str(&p.language);
                    out.push('\n');
                } else {
                    out.push_str("```\n");
                }
                out.push_str(&p.content);
                out.push_str("\n```");
            }
            "heading" => {
                let n = p.heading_level.max(1).min(6) as usize;
                out.push_str(&"#".repeat(n));
                out.push(' ');
                out.push_str(&p.content);
            }
            "list-item" => {
                out.push_str(&"  ".repeat(p.heading_level.max(0) as usize));
                out.push_str("- ");
                out.push_str(&p.content);
            }
            "block-quote" => {
                out.push_str("> ");
                out.push_str(&p.content.replace('\n', "\n> "));
            }
            "separator" => out.push_str("---"),
            "inline-code" => {
                out.push('`');
                out.push_str(&p.content);
                out.push('`');
            }
            "table-row" => {
                out.push_str(&p.cells.join(" | "));
            }
            _ => out.push_str(&p.content),
        }
    }
    out
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
            | "inline-code" | "table-row" if is_user_block => {
                if user_blocks.is_empty() {
                    // First user block wasn't "user" type (e.g. code-only)
                    user_blocks.push(p.clone());
                } else {
                    user_blocks.push(p.clone());
                }
            }
            "assistant" | "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row" => {
                if !assistant_buf.is_empty() {
                    assistant_buf.push('\n');
                }
                assistant_buf.push_str(&p.content);
            }
            "tool-call" => {
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
                    tool_call_id: id,
                    name: p.tool_name.clone(),
                    arguments,
                });
            }
            "tool-result" => {
                if let Some(id) = last_tool_call_id.take() {
                    turns.push(TurnRecord::ToolResult {
                        tool_call_id: id,
                        content: p.content.clone(),
                    });
                }
            }
            "thinking" => {
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
            _ => {}
        }
    }
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
