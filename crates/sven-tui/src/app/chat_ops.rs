// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat display rendering, scroll management, and segment synchronisation helpers.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::style::Style;
use sven_model::{MessageContent, Role};
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
    history_save, history_save_to,
    markdown::render_markdown,
    serialize_jsonl_records,
    ui::theme::{BAR_AGENT, BAR_THINKING},
    ConversationRecord,
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
        let tool_start_times = self.agent.tool_start_times.clone();
        let tool_streaming_content = self.chat.tool_streaming_content.clone();
        let anim_frame = self.agent.anim_frame;
        let segs_len = self.chat.segments.len();

        // Track result segments that have been visually merged into a grouped pair
        // line so their loop iteration can be skipped without rendering a duplicate.
        let mut grouped_result_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for i in 0..segs_len {
            // ── Grouped result: already rendered as part of the preceding ToolCall.
            // Push a zero-height range to keep segment ↔ range-index alignment.
            if grouped_result_indices.contains(&i) {
                ranges.push((line_start, line_start));
                continue;
            }

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

            // Mark the result as consumed so its own loop iteration only
            // records a zero-height range instead of rendering a duplicate line.
            if let Some(result_idx) = paired_result_idx {
                grouped_result_indices.insert(result_idx);
            }

            // Extract streaming content for in-progress sub-agent tool calls.
            let streaming_preview = extract_call_id(seg)
                .and_then(|id| tool_streaming_content.get(id))
                .cloned();

            let s = if let Some(result_idx) = paired_result_idx {
                // Both the tool call (i) and result (result_idx) are tier-0:
                // render as a single grouped line.
                let result_seg = &self.chat.segments[result_idx];
                make_grouped_preview(seg, result_seg, &self.chat.tool_args, &tool_durations)
            } else if expand == 0 {
                // In-progress tool call: animate with scanning dot.
                animated_tool_preview(seg, &tool_start_times, anim_frame, ascii).unwrap_or_else(
                    || collapsed_preview(seg, &self.chat.tool_args, &tool_durations),
                )
            } else if expand == 1 {
                // Tier-1: show either live streaming content (for running sub-agents)
                // or the standard partial content view.
                if let Some(ref content) = streaming_preview {
                    format_streaming_preview(seg, &self.chat.tool_args, content, PARTIAL_VIEW_LINES)
                } else {
                    partial_content(seg, &self.chat.tool_args, PARTIAL_VIEW_LINES)
                }
            } else {
                // Tier-2: full content or full streaming output.
                if let Some(ref content) = streaming_preview {
                    format_streaming_preview(seg, &self.chat.tool_args, content, usize::MAX)
                } else {
                    segment_to_markdown(seg, &self.chat.tool_args)
                }
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

            // Only insert action labels when the segment is expanded (tier ≥ 1)
            // or is the currently focused segment.  Collapsed tier-0 segments
            // should not show icons — clicking them cycles expand level instead.
            let is_focused = self.chat.focused_segment == Some(i);
            if self.nvim.disabled && (expand >= 1 || is_focused) {
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
        }

        if !self.chat.streaming_buffer.is_empty() {
            let (s, bar_color) = if self.chat.streaming_is_thinking {
                // Oscilloscope wave shifts every anim_frame tick (clock-driven).
                let wave = crate::ui::theme::thinking_wave(anim_frame, ascii);
                let preview = first_words(&self.chat.streaming_buffer, 8);
                let sep = if self.chat.segments.is_empty() {
                    ""
                } else {
                    "\n"
                };
                let text = format!("{sep}{SYM_THINK} **Seasoning**  {wave}  `{preview}`");
                (text, Some(Style::default().fg(BAR_THINKING)))
            } else {
                // Blinking ▌ cursor shows the stream is live.
                let cursor = crate::ui::theme::stream_cursor(anim_frame, ascii);
                let sep = if self.chat.segments.is_empty() {
                    ""
                } else {
                    "\n"
                };
                let text = format!("{sep}**Agent:** {}{}", self.chat.streaming_buffer, cursor);
                (text, Some(Style::default().fg(BAR_AGENT)))
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
        let records: Vec<ConversationRecord> = self
            .chat
            .segments
            .iter()
            .filter_map(|seg| match seg {
                ChatSegment::Message(m) => Some(ConversationRecord::Message(m.clone())),
                ChatSegment::Thinking { content } => Some(ConversationRecord::Thinking {
                    content: content.clone(),
                }),
                ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                    strategy,
                    turn,
                } => Some(ConversationRecord::ContextCompacted {
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    strategy: Some(strategy.to_string()),
                    turn: Some(*turn),
                }),
                ChatSegment::Error(_) => None,
                // Collab events and delegate summaries are display-only; skip.
                ChatSegment::CollabEvent(_) => None,
                ChatSegment::DelegateSummary { .. } => None,
            })
            .collect();

        if records.is_empty() {
            return;
        }

        let messages: Vec<sven_model::Message> = records
            .iter()
            .filter_map(|r| {
                if let ConversationRecord::Message(m) = r {
                    Some(m.clone())
                } else {
                    None
                }
            })
            .collect();

        if let Some(jsonl_path) = self.jsonl_path.clone() {
            let serialized = serialize_jsonl_records(&records);
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
            None => match history_save(&messages) {
                Ok(path) => {
                    debug!(path = %path.display(), "conversation saved to history");
                    self.history_path = Some(path);
                }
                Err(e) => debug!("failed to save conversation to history: {e}"),
            },
            Some(path) => {
                tokio::spawn(async move {
                    if let Err(e) = history_save_to(&path, &messages) {
                        debug!("failed to update conversation history: {e}");
                    }
                });
            }
        }
    }

    /// Start a completely new conversation with a fresh JSONL file.
    pub(crate) async fn start_new_conversation(&mut self) {
        // Clear current chat segments
        self.chat.segments.clear();
        self.chat.tool_args.clear();

        // Generate a new JSONL path
        if let Some(new_path) = sven_runtime::resolve_auto_log_path() {
            self.jsonl_path = Some(new_path.clone());
            tracing::info!(path = %new_path.display(), "started new conversation");

            // Save empty state to the new file
            let records: Vec<ConversationRecord> = vec![];
            let serialized = serialize_jsonl_records(&records);
            if let Err(e) = std::fs::write(&new_path, &serialized) {
                tracing::warn!("failed to create new JSONL file: {e}");
            }
        } else {
            tracing::warn!("could not resolve new log path - keeping current JSONL path");
        }

        // Also clear history path to start fresh
        self.history_path = None;

        self.rerender_chat().await;
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

// ── In-progress animation helper ─────────────────────────────────────────────

/// If `seg` is a ToolCall whose `call_id` is still tracked in
/// `tool_start_times` (i.e. the tool hasn't finished yet), return an animated
/// single-line markdown preview using the scanning-dot animation and a live
/// elapsed-time counter.  Otherwise returns `None`.
fn animated_tool_preview(
    seg: &ChatSegment,
    tool_start_times: &HashMap<String, Instant>,
    anim_frame: u8,
    ascii: bool,
) -> Option<String> {
    let (call_id, name) = match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (
                Role::Assistant,
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                },
            ) => (tool_call_id.as_str(), function.name.as_str()),
            _ => return None,
        },
        _ => return None,
    };

    let start = tool_start_times.get(call_id)?;
    let elapsed = start.elapsed().as_secs_f32();
    let scan = crate::ui::theme::tool_scan(anim_frame, ascii);
    Some(format!(
        "\n**Agent:tool_call:{call_id}**\n{SYM_TOOL} **{name}**  {scan}  {elapsed:.1}s\n"
    ))
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

// ── Mouse selection clipboard copy ───────────────────────────────────────────

impl App {
    /// Copy the currently selected text (from a mouse drag selection) to the
    /// terminal clipboard via OSC 52.  Shows a toast on success.
    pub(crate) fn copy_selection_to_clipboard(&mut self) {
        let Some((s_line, s_col, e_line, e_col)) = self.chat.normalized_selection() else {
            return;
        };
        let text = extract_selection_text(&self.chat.lines, s_line, s_col, e_line, e_col);
        if !text.is_empty() {
            osc52_copy(&text);
            self.ui
                .push_toast(crate::app::ui_state::Toast::info("Selection copied"));
        }
    }
}

/// Extract visible text from a range of rendered chat lines, respecting column
/// boundaries for the first and last line of the selection.
fn extract_selection_text(
    lines: &crate::markdown::StyledLines,
    start_line: usize,
    start_col: u16,
    end_line: usize,
    end_col: u16,
) -> String {
    let line_count = lines.len();
    if line_count == 0 {
        return String::new();
    }
    let e_line = end_line.min(line_count - 1);
    let mut result = String::new();
    #[allow(clippy::needless_range_loop)]
    for abs_line in start_line..=e_line {
        let line = &lines[abs_line];
        let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let char_count = line_text.chars().count();
        let from_col = if abs_line == start_line {
            start_col as usize
        } else {
            0
        };
        let to_col = if abs_line == end_line {
            (end_col as usize).min(char_count)
        } else {
            char_count
        };
        let extracted: String = line_text
            .chars()
            .enumerate()
            .filter(|(i, _)| *i >= from_col && *i < to_col)
            .map(|(_, c)| c)
            .collect();
        if abs_line > start_line {
            result.push('\n');
        }
        result.push_str(&extracted);
    }
    // Trim trailing whitespace per line, then overall trailing newlines.
    let trimmed: Vec<String> = result.lines().map(|l| l.trim_end().to_string()).collect();
    trimmed.join("\n").trim_end().to_string()
}

// ── Streaming helpers ─────────────────────────────────────────────────────────

/// Extract the first N whitespace-separated words from text.
fn first_words(text: &str, n: usize) -> String {
    text.split_whitespace()
        .take(n)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the tool-call ID from a segment, if it is an in-progress tool call.
fn extract_call_id(seg: &ChatSegment) -> Option<&str> {
    use crate::chat::segment::ChatSegment;
    use sven_model::{MessageContent, Role};
    match seg {
        ChatSegment::Message(m) if m.role == Role::Assistant => {
            if let MessageContent::ToolCall { tool_call_id, .. } = &m.content {
                Some(tool_call_id.as_str())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Render the tool call args header followed by the streaming output content.
///
/// `max_lines` limits how many lines of streaming content to show (for tier-1
/// partial view).  Pass `usize::MAX` for the full view.
fn format_streaming_preview(
    seg: &ChatSegment,
    tool_args: &std::collections::HashMap<String, String>,
    content: &str,
    max_lines: usize,
) -> String {
    // Start with the standard segment header (tool name + args).
    let header = segment_to_markdown(seg, tool_args);

    // Parse "lines:<n>" status line from content if present.
    let (status_line, output_start) = if let Some(first_line) = content.lines().next() {
        if first_line.starts_with("lines:") {
            (first_line, content.lines().skip(1).collect::<Vec<_>>())
        } else {
            ("", content.lines().collect::<Vec<_>>())
        }
    } else {
        ("", vec![])
    };

    let status_suffix = if !status_line.is_empty() {
        format!(" — {}", status_line)
    } else {
        String::new()
    };

    let tail_lines: Vec<&str> = if max_lines == usize::MAX {
        output_start.clone()
    } else {
        let start = output_start.len().saturating_sub(max_lines);
        output_start[start..].to_vec()
    };

    if tail_lines.is_empty() {
        format!(
            "{}\n\n**▶ Streaming output{}** *(no output yet)*",
            header, status_suffix
        )
    } else {
        format!(
            "{}\n\n**▶ Streaming output{}**\n```\n{}\n```",
            header,
            status_suffix,
            tail_lines.join("\n")
        )
    }
}
