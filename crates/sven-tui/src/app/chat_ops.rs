// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat display rendering, scroll management, and segment synchronisation helpers.

use ratatui::style::Style;
use sven_model::MessageContent;
use tracing::debug;

use crate::{
    app::App,
    chat::{
        markdown::{
            apply_bar_and_dim, apply_focused_bar, collapsed_preview, format_conversation,
            parse_markdown_to_messages, partial_content, segment_bar_style, segment_to_markdown,
            SYM_THINK, SYM_TOOL,
        },
        segment::{
            segment_at_line, segment_editable_text, segment_is_removable, segment_is_rerunnable,
            ChatSegment,
        },
    },
    markdown::render_markdown,
    ui::theme::{BAR_AGENT, BAR_THINKING},
};

/// Number of lines to show in tier-1 (partial) view.
const PARTIAL_VIEW_LINES: usize = 12;

impl App {
    // ── Chat display ──────────────────────────────────────────────────────────

    /// Rebuild `chat.lines` and `chat.segment_line_ranges` from `chat.segments`
    /// plus the streaming buffer.
    pub(crate) fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges = Vec::new();
        let mut edit_labels: std::collections::HashSet<usize> = Default::default();
        let mut remove_labels: std::collections::HashSet<usize> = Default::default();
        let mut rerun_labels: std::collections::HashSet<usize> = Default::default();
        let mut copy_labels: std::collections::HashSet<usize> = Default::default();
        let mut line_start = 0usize;
        let ascii = self.ascii();
        let bar_char = if ascii { "| " } else { "▌ " };
        let bar_cols: u16 = unicode_width::UnicodeWidthStr::width_cjk(bar_char) as u16;
        // Reserve space for action labels: ↻ ✎ ✕ y  = 9 chars (+ 1 spare)
        let label_reserve: u16 = if self.nvim.disabled { 10 } else { 0 };
        let effective_width = self
            .layout
            .chat_inner_width
            .saturating_sub(bar_cols + label_reserve)
            .max(20);
        let render_width = if self.config.tui.wrap_width == 0 {
            effective_width
        } else {
            self.config.tui.wrap_width.min(effective_width)
        };

        let tool_durations = self.chat.tool_durations.clone();
        let segs_len = self.chat.segments.len();

        for i in 0..segs_len {
            let seg = &self.chat.segments[i];
            let expand = if self.nvim.disabled {
                self.chat.effective_expand_level(i, seg)
            } else {
                2 // always full in nvim mode
            };

            // Check if this is a ToolCall paired with the immediately following
            // ToolResult — when both are at tier 0, render as a single grouped line.
            let paired_result_idx = if expand == 0 {
                get_paired_result_idx(&self.chat.segments, i)
            } else {
                None
            };

            let s = if let Some(result_idx) = paired_result_idx {
                // Both the tool call (i) and result (result_idx) are tier-0:
                // render as a single grouped line. The result segment will be
                // skipped below.
                let result_seg = &self.chat.segments[result_idx];
                make_grouped_preview(seg, result_seg, &self.chat.tool_args, &tool_durations)
            } else if expand == 0 {
                collapsed_preview(seg, &self.chat.tool_args, &tool_durations)
            } else if expand == 1 {
                partial_content(seg, &self.chat.tool_args, PARTIAL_VIEW_LINES)
            } else {
                segment_to_markdown(seg, &self.chat.tool_args)
            };

            let lines = render_markdown(&s, render_width, ascii);
            let (bar_style, dim) = segment_bar_style(seg);
            let mut styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);

            // Highlight the bar for the focused segment.
            if self.nvim.disabled {
                if let Some(focused) = self.chat.focused_segment {
                    if focused == i {
                        styled = apply_focused_bar(styled, bar_char);
                    }
                }
            }

            let n = styled.len();

            if self.nvim.disabled {
                if segment_editable_text(&self.chat.segments, i).is_some() {
                    edit_labels.insert(line_start);
                }
                if segment_is_removable(seg) {
                    remove_labels.insert(line_start);
                }
                if segment_is_rerunnable(seg) {
                    rerun_labels.insert(line_start);
                }
                copy_labels.insert(line_start);
            }

            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;

            // If we rendered a grouped pair, add an empty range for the result segment.
            if let Some(result_idx) = paired_result_idx {
                // The result segment is visually merged with the call — give it a
                // zero-height range so click detection still resolves it.
                while ranges.len() <= result_idx {
                    ranges.push((line_start, line_start));
                }
                // Overwrite the result slot with the current line position.
                ranges[result_idx] = (line_start, line_start);
            }
        }

        if !self.chat.streaming_buffer.is_empty() {
            let (s, bar_color) = if self.chat.streaming_is_thinking {
                let spinner = crate::ui::theme::spinner_char(self.agent.spinner_frame, ascii);
                let preview = first_words(&self.chat.streaming_buffer, 8);
                let prefix = if self.chat.segments.is_empty() {
                    format!("{SYM_THINK} **Seasoning…** {spinner}  `{preview}`")
                } else {
                    format!("\n{SYM_THINK} **Seasoning…** {spinner}  `{preview}`")
                };
                (prefix, Some(Style::default().fg(BAR_THINKING)))
            } else {
                let prefix = if self.chat.segments.is_empty() {
                    "**Agent:** "
                } else {
                    "\n**Agent:** "
                };
                (
                    format!("{}{}", prefix, self.chat.streaming_buffer),
                    Some(Style::default().fg(BAR_AGENT)),
                )
            };
            let lines = render_markdown(&s, render_width, ascii);
            let styled = apply_bar_and_dim(lines, bar_color, false, bar_char);
            all_lines.extend(styled);
        }

        self.chat.lines = all_lines;
        self.chat.segment_line_ranges = ranges;
        self.chat.edit_labels = edit_labels;
        self.chat.remove_labels = remove_labels;
        self.chat.rerun_labels = rerun_labels;
        self.chat.copy_labels = copy_labels;
        self.recompute_focused_segment();
    }

    /// Recompute the keyboard-focused segment based on the current scroll
    /// offset and chat height.
    pub(crate) fn recompute_focused_segment(&mut self) {
        let center = self.chat.scroll_offset as usize + self.layout.chat_height as usize / 2;
        self.chat.focused_segment = segment_at_line(&self.chat.segment_line_ranges, center)
            .filter(|&idx| segment_editable_text(&self.chat.segments, idx).is_some());
    }

    /// Re-render the chat pane: update the Neovim buffer (if active) and
    /// rebuild the ratatui display lines.
    pub(crate) async fn rerender_chat(&mut self) {
        if let Some(nvim_bridge) = &self.nvim.bridge {
            let content = format_conversation(
                &self.chat.segments,
                &self.chat.streaming_buffer,
                &self.chat.tool_args,
            );
            let mut bridge = nvim_bridge.lock().await;
            if let Err(e) = bridge.set_modifiable(true).await {
                tracing::error!("Failed to set buffer modifiable for update: {}", e);
            }
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
            if self.agent.busy {
                if let Err(e) = bridge.set_modifiable(false).await {
                    tracing::error!("Failed to set buffer non-modifiable: {}", e);
                }
            }
        }
        self.build_display_from_segments();
        self.ui.search.update_matches(&self.chat.lines);
    }

    pub(crate) fn ascii(&self) -> bool {
        if std::env::var("SVEN_ASCII_BORDERS").as_deref() == Ok("1") {
            return true;
        }
        self.config.tui.ascii_borders
    }

    // ── Scroll helpers ────────────────────────────────────────────────────────

    pub(crate) fn scroll_up(&mut self, n: u16) {
        self.chat.scroll_offset = self.chat.scroll_offset.saturating_sub(n);
        self.chat.auto_scroll = false;
        self.recompute_focused_segment();
    }

    pub(crate) fn scroll_down(&mut self, n: u16) {
        let max = (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
        self.chat.scroll_offset = (self.chat.scroll_offset + n).min(max);
        if self.chat.scroll_offset >= max {
            self.chat.auto_scroll = true;
        }
        self.recompute_focused_segment();
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        if self.nvim.bridge.is_none() && self.chat.auto_scroll {
            self.chat.scroll_offset =
                (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
        }
        self.recompute_focused_segment();
    }

    /// Adjust `input.scroll_offset` so the cursor row is within the visible
    /// window of the input pane.
    pub(crate) fn adjust_input_scroll(&mut self) {
        let w = self.layout.input_inner_width as usize;
        let h = self.layout.input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
        let effective_wrap = if wrap.lines.len() > h && w > 1 {
            crate::input_wrap::wrap_content(&self.input.buffer, w - 1, self.input.cursor)
        } else {
            wrap
        };
        crate::input_wrap::adjust_scroll(
            effective_wrap.cursor_row,
            h,
            &mut self.input.scroll_offset,
        );
    }

    /// Adjust `edit.scroll_offset` so the cursor row is within the visible
    /// window when in inline edit mode.
    pub(crate) fn adjust_edit_scroll(&mut self) {
        let w = self.layout.input_inner_width as usize;
        let h = self.layout.input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(&self.edit.buffer, w, self.edit.cursor);
        let effective_wrap = if wrap.lines.len() > h && w > 1 {
            crate::input_wrap::wrap_content(&self.edit.buffer, w - 1, self.edit.cursor)
        } else {
            wrap
        };
        crate::input_wrap::adjust_scroll(
            effective_wrap.cursor_row,
            h,
            &mut self.edit.scroll_offset,
        );
    }

    // ── History persistence ───────────────────────────────────────────────────

    pub(crate) fn save_history_async(&mut self) {
        let records: Vec<sven_input::ConversationRecord> = self
            .chat
            .segments
            .iter()
            .filter_map(|seg| match seg {
                ChatSegment::Message(m) => Some(sven_input::ConversationRecord::Message(m.clone())),
                ChatSegment::Thinking { content } => {
                    Some(sven_input::ConversationRecord::Thinking {
                        content: content.clone(),
                    })
                }
                ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                    strategy,
                    turn,
                } => Some(sven_input::ConversationRecord::ContextCompacted {
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    strategy: Some(strategy.to_string()),
                    turn: Some(*turn),
                }),
                ChatSegment::Error(_) => None,
            })
            .collect();

        if records.is_empty() {
            return;
        }

        let messages: Vec<sven_model::Message> = records
            .iter()
            .filter_map(|r| {
                if let sven_input::ConversationRecord::Message(m) = r {
                    Some(m.clone())
                } else {
                    None
                }
            })
            .collect();

        if let Some(jsonl_path) = self.jsonl_path.clone() {
            let serialized = sven_input::serialize_jsonl_records(&records);
            tokio::spawn(async move {
                if let Err(e) = std::fs::write(&jsonl_path, &serialized) {
                    tracing::debug!("failed to update JSONL conversation file: {e}");
                }
            });
        }

        if messages.is_empty() {
            return;
        }

        let path_opt = self.history_path.clone();
        match path_opt {
            None => match sven_input::history::save(&messages) {
                Ok(path) => {
                    debug!(path = %path.display(), "conversation saved to history");
                    self.history_path = Some(path);
                }
                Err(e) => debug!("failed to save conversation to history: {e}"),
            },
            Some(path) => {
                tokio::spawn(async move {
                    if let Err(e) = sven_input::history::save_to(&path, &messages) {
                        debug!("failed to update conversation history: {e}");
                    }
                });
            }
        }
    }

    // ── Neovim sync ───────────────────────────────────────────────────────────

    pub(crate) async fn sync_nvim_buffer_to_segments(&mut self) {
        let content = if let Some(nvim_bridge) = &self.nvim.bridge {
            let bridge = nvim_bridge.lock().await;
            bridge.get_buffer_content().await.ok()
        } else {
            return;
        };
        if let Some(content) = content {
            match parse_markdown_to_messages(&content) {
                Ok(messages) if !messages.is_empty() => {
                    self.chat.segments = messages
                        .iter()
                        .map(|m| ChatSegment::Message(m.clone()))
                        .collect();
                    self.chat.tool_args.clear();
                    for m in &messages {
                        if let MessageContent::ToolCall {
                            tool_call_id,
                            function,
                        } = &m.content
                        {
                            self.chat
                                .tool_args
                                .insert(tool_call_id.clone(), function.name.clone());
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("sync_nvim_buffer_to_segments: parse error — keeping existing: {e}");
                }
            }
        }
    }

    pub(crate) async fn nvim_scroll_to_bottom(&self) {
        if let Some(nvim_bridge) = &self.nvim.bridge {
            let mut bridge = nvim_bridge.lock().await;
            let _ = bridge.send_input("G").await;
        }
    }

    // ── Clipboard copy ────────────────────────────────────────────────────────

    /// Copy the text content of a segment to the terminal clipboard via OSC 52.
    pub(crate) fn copy_segment_to_clipboard(&self, seg_idx: usize) -> bool {
        if let Some(seg) = self.chat.segments.get(seg_idx) {
            let text = extract_segment_text(seg, &self.chat.tool_args);
            if !text.is_empty() {
                osc52_copy(&text);
                return true;
            }
        }
        false
    }

    /// Copy all chat content to the clipboard via OSC 52.
    pub(crate) fn copy_all_to_clipboard(&self) -> bool {
        if self.chat.segments.is_empty() {
            return false;
        }
        let text = format_conversation(&self.chat.segments, "", &self.chat.tool_args);
        osc52_copy(&text);
        true
    }
}

// ── Tool-call pair helpers ────────────────────────────────────────────────────

/// If segment `i` is a ToolCall and segment `i+1` is its ToolResult (same
/// call_id), and both are at default expand level (0), return the index of
/// the result. Otherwise return `None`.
fn get_paired_result_idx(segments: &[ChatSegment], i: usize) -> Option<usize> {
    let call_id = match segments.get(i) {
        Some(ChatSegment::Message(m)) => match &m.content {
            MessageContent::ToolCall { tool_call_id, .. } => tool_call_id.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    let next_idx = i + 1;
    match segments.get(next_idx) {
        Some(ChatSegment::Message(m)) => match &m.content {
            MessageContent::ToolResult { tool_call_id, .. } if tool_call_id == call_id => {
                Some(next_idx)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Build a single-line grouped preview for a ToolCall + ToolResult pair.
fn make_grouped_preview(
    call_seg: &ChatSegment,
    result_seg: &ChatSegment,
    _tool_args: &std::collections::HashMap<String, String>,
    tool_durations: &std::collections::HashMap<String, f32>,
) -> String {
    use crate::chat::markdown::{compact_args_summary_pub, SYM_ERR, SYM_EXPAND, SYM_OK};

    let (call_id, name, args) = match call_seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => (
                tool_call_id.as_str(),
                function.name.as_str(),
                function.arguments.as_str(),
            ),
            _ => return String::new(),
        },
        _ => return String::new(),
    };

    let (result_content, is_error) = match result_seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolResult { content, .. } => {
                let s = content.to_string();
                let err = s.starts_with("error:");
                (s, err)
            }
            _ => (String::new(), false),
        },
        _ => (String::new(), false),
    };

    let args_sum = compact_args_summary_pub(args, 1, 30);
    let result_preview: String = result_content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(40)
        .collect();
    let result_sym = if is_error { SYM_ERR } else { SYM_OK };
    let duration = tool_durations
        .get(call_id)
        .map(|s| format!("  {:.1}s", s))
        .unwrap_or_default();

    let args_part = if args_sum.is_empty() {
        String::new()
    } else {
        format!("  {args_sum}")
    };
    let result_part = if result_preview.is_empty() {
        String::new()
    } else {
        format!("  → {result_sym} `{result_preview}`")
    };

    format!(
        "\n**Agent:tool_call:{call_id}**\n{SYM_TOOL} **{name}**{args_part}{result_part}{duration}  {SYM_EXPAND}\n"
    )
}

// ── Clipboard via OSC 52 ──────────────────────────────────────────────────────

/// Copy `text` to the terminal clipboard using the OSC 52 escape sequence.
/// Works in most modern terminals (kitty, alacritty, tmux with allow-passthrough,
/// iTerm2, foot, wezterm) without any native library dependency.
fn osc52_copy(text: &str) {
    use std::io::Write;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text);
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = std::io::stdout().write_all(seq.as_bytes());
    let _ = std::io::stdout().flush();
}

// ── Text extraction for clipboard ────────────────────────────────────────────

fn extract_segment_text(
    seg: &ChatSegment,
    tool_args: &std::collections::HashMap<String, String>,
) -> String {
    match seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::ToolCall { function, .. } => {
                let pretty = serde_json::from_str::<serde_json::Value>(&function.arguments)
                    .and_then(|v| serde_json::to_string_pretty(&v))
                    .unwrap_or_else(|_| function.arguments.clone());
                format!("Tool: {}\n{}", function.name, pretty)
            }
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => {
                let name = tool_args
                    .get(tool_call_id)
                    .map(|s| s.as_str())
                    .unwrap_or("tool");
                format!("Result: {name}\n{content}")
            }
            _ => String::new(),
        },
        ChatSegment::Thinking { content } => content.clone(),
        ChatSegment::Error(msg) => format!("Error: {msg}"),
        _ => String::new(),
    }
}

// ── Streaming helpers ─────────────────────────────────────────────────────────

/// Extract the first N whitespace-separated words from text.
fn first_words(text: &str, n: usize) -> String {
    text.split_whitespace()
        .take(n)
        .collect::<Vec<_>>()
        .join(" ")
}
