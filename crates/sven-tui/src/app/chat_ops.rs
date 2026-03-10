// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat display rendering, scroll management, and segment synchronisation helpers.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use sven_model::{MessageContent, Role};
use tracing::debug;

use crate::{
    app::App,
    chat::{
        markdown::{
            apply_bar_and_dim, collapsed_preview, format_conversation, parse_markdown_to_messages,
            partial_content, segment_bar_style, segment_to_markdown, strip_display_anchors,
            ToolDisplayRegistryRef, SYM_THINK, SYM_TOOL,
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
    ui::tool_renderer,
    ui::width_utils::{col_to_byte_offset, display_width, truncate_to_width},
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
        let bar_cols: u16 = unicode_width::UnicodeWidthStr::width(bar_char) as u16;
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
        let tool_display_registry: ToolDisplayRegistryRef = self.shared_tool_displays.get();
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

            // Determine if this is a tool call/result segment eligible for rich rendering.
            let rich_lines_opt = render_segment_rich(
                seg,
                paired_result_idx.and_then(|ri| self.chat.segments.get(ri)),
                expand,
                &self.chat.tool_args,
                &tool_durations,
                &tool_start_times,
                &tool_streaming_content,
                anim_frame,
                ascii,
                tool_display_registry.clone(),
                render_width,
                bar_char,
            );

            let styled = if let Some(rich) = rich_lines_opt {
                // Use rich ratatui rendering for tool calls/results.
                rich
            } else {
                let s = if let Some(result_idx) = paired_result_idx {
                    // Both the tool call (i) and result (result_idx) are tier-0:
                    // render as a single grouped line.
                    let result_seg = &self.chat.segments[result_idx];
                    make_grouped_preview(
                        seg,
                        result_seg,
                        &self.chat.tool_args,
                        &tool_durations,
                        tool_display_registry.clone(),
                    )
                } else if expand == 0 {
                    // In-progress tool call: animate with scanning dot.
                    animated_tool_preview(
                        seg,
                        &tool_start_times,
                        &tool_streaming_content,
                        anim_frame,
                        ascii,
                        tool_display_registry.clone(),
                    )
                    .unwrap_or_else(|| {
                        collapsed_preview(
                            seg,
                            &self.chat.tool_args,
                            &tool_durations,
                            tool_display_registry.clone(),
                        )
                    })
                } else if expand == 1 {
                    let raw = if let Some(ref content) = streaming_preview {
                        format_streaming_preview(
                            seg,
                            &self.chat.tool_args,
                            content,
                            PARTIAL_VIEW_LINES,
                        )
                    } else {
                        partial_content(seg, &self.chat.tool_args, PARTIAL_VIEW_LINES)
                    };
                    strip_display_anchors(&raw)
                } else {
                    let raw = if let Some(ref content) = streaming_preview {
                        format_streaming_preview(seg, &self.chat.tool_args, content, usize::MAX)
                    } else {
                        segment_to_markdown(seg, &self.chat.tool_args)
                    };
                    strip_display_anchors(&raw)
                };

                let lines = render_markdown(&s, render_width, ascii);
                let (bar_style, dim) = segment_bar_style(seg);
                apply_bar_and_dim(lines, bar_style, dim, bar_char)
            };

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
                // Scanning dot (side to side) for Seasoning, same as in-progress tool calls.
                let dot = crate::ui::theme::tool_scan(anim_frame, ascii);
                let normalized: String = self
                    .chat
                    .streaming_buffer
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let preview = truncate_to_width(&normalized, 80);
                let sep = if self.chat.segments.is_empty() {
                    ""
                } else {
                    "\n"
                };
                let text = format!("{sep}{SYM_THINK} **Seasoning**  {dot}  `{preview}`");
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
        // Keep existing highlight if still valid; otherwise set from center (e.g. first load).
        if self.chat.focused_segment.is_none_or(|i| i >= segs_len) {
            self.recompute_focused_segment();
        }
    }

    /// If segment `idx` is a ToolCall, return the index of the immediately
    /// following ToolResult with the same call_id (if any).
    pub(crate) fn paired_result_for(&self, idx: usize) -> Option<usize> {
        get_paired_result_idx(&self.chat.segments, idx)
    }

    /// Recompute the keyboard-focused segment (highlight) based on the current
    /// scroll offset and chat height. Used when focus moves to the chat pane.
    pub(crate) fn recompute_focused_segment(&mut self) {
        let center = self.chat.scroll_offset as usize + self.layout.chat_height as usize / 2;
        self.chat.focused_segment = segment_at_line(&self.chat.segment_line_ranges, center);
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
    }

    pub(crate) fn scroll_down(&mut self, n: u16) {
        let max = (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
        self.chat.scroll_offset = (self.chat.scroll_offset + n).min(max);
        if self.chat.scroll_offset >= max {
            self.chat.auto_scroll = true;
        }
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        if self.nvim.bridge.is_none() && self.chat.auto_scroll {
            self.chat.scroll_offset =
                (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
        }
    }

    /// Adjust scroll so the segment at `seg_idx` is visible. Called after j/k move the highlight.
    pub(crate) fn scroll_chat_to_show_segment(&mut self, seg_idx: usize) {
        let Some(&(seg_start, seg_end)) = self.chat.segment_line_ranges.get(seg_idx) else {
            return;
        };
        let h = self.layout.chat_height as usize;
        let seg_start_u = seg_start as u16;
        if seg_start_u < self.chat.scroll_offset {
            self.chat.scroll_offset = seg_start_u;
        } else if seg_end > self.chat.scroll_offset as usize + h {
            self.chat.scroll_offset = (seg_end.saturating_sub(h)) as u16;
        }
        self.chat.auto_scroll = false;
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

    /// Synchronous variant of `save_history_async` for use at clean exit.
    ///
    /// Called just before `run()` returns so that any messages typed in the
    /// current session are written to disk even if the tokio runtime is about
    /// to drop (which would cancel any pending `tokio::spawn` write tasks).
    pub(crate) fn save_history_sync(&mut self) {
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
                _ => None,
            })
            .collect();

        if records.is_empty() {
            return;
        }

        let yaml_path = self.yaml_path.clone();
        let model = Some(self.session.model_display.clone());
        let mode = Some(self.session.mode.to_string());
        let active_id = self.sessions.active_id.clone();
        let mut doc = if let Some(entry) = self.sessions.get(&active_id) {
            entry.to_document(&self.chat, model, mode)
        } else {
            let turns = sven_input::records_to_turns(&records);
            sven_input::ChatDocument {
                id: active_id,
                title: self.chat_title.clone(),
                model,
                mode,
                status: sven_input::ChatStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                turns,
            }
        };

        let result = if let Some(ref path) = yaml_path {
            sven_input::save_chat_to(path, &mut doc)
        } else {
            sven_input::save_chat(&mut doc)
        };
        if let Err(e) = result {
            tracing::debug!("failed to save YAML chat document on exit: {e}");
        }
    }

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
                // Display-only segments: never persisted to JSONL / history.
                ChatSegment::TodoUpdate(_) => None,
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

        // Save as YAML chat document, preserving the original created_at timestamp.
        {
            let yaml_path = self.yaml_path.clone();
            let model = Some(self.session.model_display.clone());
            let mode = Some(self.session.mode.to_string());
            let active_id = self.sessions.active_id.clone();
            let mut doc = if let Some(entry) = self.sessions.get(&active_id) {
                entry.to_document(&self.chat, model, mode)
            } else {
                // Fallback for the rare case where the active entry isn't found.
                let turns = sven_input::records_to_turns(&records);
                sven_input::ChatDocument {
                    id: active_id,
                    title: self.chat_title.clone(),
                    model,
                    mode,
                    status: sven_input::ChatStatus::Active,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    turns,
                }
            };
            tokio::spawn(async move {
                let result = if let Some(ref path) = yaml_path {
                    sven_input::save_chat_to(path, &mut doc)
                } else {
                    sven_input::save_chat(&mut doc)
                };
                if let Err(e) = result {
                    tracing::debug!("failed to save YAML chat document: {e}");
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
    tool_streaming_content: &HashMap<String, String>,
    anim_frame: u8,
    ascii: bool,
    tool_display_registry: ToolDisplayRegistryRef,
) -> Option<String> {
    use crate::chat::markdown::tool_smart_summary;

    let (call_id, name, args) = match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (
                Role::Assistant,
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                },
            ) => (
                tool_call_id.as_str(),
                function.name.as_str(),
                function.arguments.as_str(),
            ),
            _ => return None,
        },
        _ => return None,
    };

    let start = tool_start_times.get(call_id)?;
    let elapsed = start.elapsed().as_secs_f32();
    let scan = crate::ui::theme::tool_scan(anim_frame, ascii);

    let args_val: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    let (label, summary) = tool_display_registry
        .as_ref()
        .and_then(|r| r.read().ok())
        .and_then(|guard| {
            guard.get(name).map(|disp| {
                (
                    disp.display_name().to_string(),
                    disp.collapsed_summary(&args_val),
                )
            })
        })
        .unwrap_or_else(|| (name.to_string(), tool_smart_summary(name, args)));
    let summary_part = if summary.is_empty() {
        String::new()
    } else {
        format!("  {summary}")
    };

    // Show the last non-empty line of streaming progress (capped to 50 cols).
    let progress_part = tool_streaming_content
        .get(call_id)
        .and_then(|content| {
            content
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|line| {
                    let trimmed = line.trim();
                    let short = truncate_to_width(trimmed, 50);
                    format!("  `{short}`")
                })
        })
        .unwrap_or_default();

    Some(format!(
        "\n{SYM_TOOL}  {label}{summary_part}{progress_part}  {scan}  {elapsed:.1}s\n"
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
    tool_display_registry: ToolDisplayRegistryRef,
) -> String {
    use crate::chat::markdown::{tool_smart_summary, SYM_ERR, SYM_EXPAND, SYM_OK};

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

    let args_val: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    let (label, summary) = tool_display_registry
        .as_ref()
        .and_then(|r| r.read().ok())
        .and_then(|guard| {
            guard.get(name).map(|disp| {
                (
                    disp.display_name().to_string(),
                    disp.collapsed_summary(&args_val),
                )
            })
        })
        .unwrap_or_else(|| (name.to_string(), tool_smart_summary(name, args)));
    let summary_part = if summary.is_empty() {
        String::new()
    } else {
        format!("  {summary}")
    };

    let is_error = match result_seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolResult { content, .. } => content.to_string().starts_with("error:"),
            _ => false,
        },
        _ => false,
    };

    let status_sym = if is_error { SYM_ERR } else { SYM_OK };
    let duration = tool_durations
        .get(call_id)
        .map(|s| format!("  {:.1}s", s))
        .unwrap_or_default();

    format!("\n{SYM_TOOL}  {label}{summary_part}  {status_sym}{duration}  {SYM_EXPAND}\n")
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
///
/// `start_col` and `end_col` are **display columns** (as reported by hit-testing),
/// not character or byte indices.  This function converts them to byte offsets
/// using cumulative unicode display width so that wide characters (emoji, CJK,
/// special symbols) are handled correctly.
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
        let line_width = display_width(&line_text);
        let from_display_col = if abs_line == start_line {
            start_col as usize
        } else {
            0
        };
        let to_display_col = if abs_line == end_line {
            (end_col as usize).min(line_width)
        } else {
            line_width
        };
        // Convert display columns to byte offsets.
        let from_byte = col_to_byte_offset(&line_text, from_display_col);
        let to_byte = col_to_byte_offset(&line_text, to_display_col);
        let extracted = &line_text[from_byte..to_byte];
        if abs_line > start_line {
            result.push('\n');
        }
        result.push_str(extracted);
    }
    // Trim trailing whitespace per line, then overall trailing newlines.
    let trimmed: Vec<String> = result.lines().map(|l| l.trim_end().to_string()).collect();
    trimmed.join("\n").trim_end().to_string()
}

// ── Streaming helpers ─────────────────────────────────────────────────────────

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

// ── Rich segment rendering ────────────────────────────────────────────────────

/// Try to render a segment using the rich `tool_renderer`, returning `Some(lines)`
/// if the segment is a tool call or result, `None` to fall back to markdown.
///
/// For tool calls/results this completely replaces the markdown pipeline
/// with styled ratatui lines that use per-tool icons, colours, and layouts.
#[allow(clippy::too_many_arguments)]
fn render_segment_rich(
    seg: &ChatSegment,
    paired_result: Option<&ChatSegment>,
    expand: u8,
    tool_args: &std::collections::HashMap<String, String>,
    tool_durations: &std::collections::HashMap<String, f32>,
    _tool_start_times: &std::collections::HashMap<String, std::time::Instant>,
    _tool_streaming_content: &std::collections::HashMap<String, String>,
    _anim_frame: u8,
    _ascii: bool,
    tool_display_registry: ToolDisplayRegistryRef,
    render_width: u16,
    bar_char: &str,
) -> Option<crate::markdown::StyledLines> {
    use crate::ui::theme::BAR_TOOL;

    let bar_style = Style::default().fg(BAR_TOOL);

    match seg {
        // ── Tool call ─────────────────────────────────────────────────────────
        ChatSegment::Message(m) if m.role == Role::Assistant => {
            if let MessageContent::ToolCall {
                tool_call_id,
                function,
            } = &m.content
            {
                let args_val: serde_json::Value =
                    serde_json::from_str(&function.arguments).unwrap_or(serde_json::Value::Null);
                let registry_guard = tool_display_registry.as_ref().and_then(|r| r.read().ok());
                let display = registry_guard
                    .as_ref()
                    .and_then(|g| g.get(function.name.as_str()));
                let duration = tool_durations.get(tool_call_id.as_str()).copied();

                if expand == 0 {
                    // ── Tier 0: single collapsed line ─────────────────────────
                    let spans = if let Some(result_seg) = paired_result {
                        // Grouped: call + result on one line.
                        build_grouped_rich_line(
                            &function.name,
                            &args_val,
                            result_seg,
                            tool_args,
                            tool_durations,
                            display,
                            tool_display_registry.clone(),
                        )
                    } else {
                        let mut spans = tool_renderer::render_tool_call_collapsed(
                            &function.name,
                            &args_val,
                            duration,
                            display,
                            render_width as usize,
                        );
                        spans.push(Span::raw("  ▶"));
                        spans
                    };

                    let mut line_spans = vec![Span::styled(bar_char.to_string(), bar_style)];
                    line_spans.extend(spans);
                    return Some(vec![Line::from(line_spans)]);
                } else {
                    // ── Tier 1/2: expanded view ───────────────────────────────
                    let mut raw_lines = tool_renderer::render_tool_call_expanded(
                        &function.name,
                        &args_val,
                        render_width,
                        display,
                    );
                    // Add a header line with the icon + name.
                    let icon = display
                        .map(|d| d.icon().to_string())
                        .unwrap_or_else(|| sven_tools::tool_icon(&function.name).to_string());
                    let display_name = display
                        .map(|d| d.display_name().to_string())
                        .unwrap_or_else(|| function.name.clone());
                    let accent = tool_renderer_accent(display, &function.name);
                    let header = Line::from(vec![
                        Span::styled(bar_char.to_string(), bar_style),
                        Span::styled(
                            format!("{icon} "),
                            Style::default()
                                .fg(accent)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            display_name,
                            Style::default()
                                .fg(accent)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]);
                    let mut result: crate::markdown::StyledLines = vec![header];
                    for l in raw_lines.drain(..) {
                        let mut spans = vec![Span::styled(bar_char.to_string(), bar_style)];
                        spans.extend(l.spans);
                        result.push(Line::from(spans));
                    }
                    // If this has a paired result in tier 1/2, append it.
                    // Paired results don't need the "Tool Result:" prefix since the
                    // parent tool call header already provides context.
                    if let Some(result_seg) = paired_result {
                        result.extend(render_tool_result_lines(
                            result_seg,
                            tool_args,
                            tool_durations,
                            tool_display_registry.clone(),
                            expand,
                            render_width,
                            bar_char,
                            bar_style,
                            None,
                        ));
                    }
                    return Some(result);
                }
            }
            None
        }

        // ── Tool result (standalone, not paired) ──────────────────────────────
        ChatSegment::Message(m) if m.role == Role::Tool => {
            if let MessageContent::ToolResult {
                tool_call_id,
                content,
            } = &m.content
            {
                let tool_name = tool_args
                    .get(tool_call_id.as_str())
                    .map(|s| s.as_str())
                    .unwrap_or("tool");
                let output_str = content.to_string();
                let is_error = output_str.starts_with("error:");
                let duration = tool_durations.get(tool_call_id.as_str()).copied();
                // Standalone results (not grouped with their call) use a "Tool Result: <name>" label.
                let standalone_label = format!("Tool Result: {tool_name}");

                if expand == 0 {
                    let registry_guard = tool_display_registry.as_ref().and_then(|r| r.read().ok());
                    let display = registry_guard.as_ref().and_then(|g| g.get(tool_name));
                    let mut spans = tool_renderer::render_tool_result_collapsed(
                        tool_name,
                        is_error,
                        duration,
                        display,
                        Some(standalone_label.clone()),
                    );
                    spans.push(Span::raw("  ▶"));
                    let mut line_spans = vec![Span::styled(bar_char.to_string(), bar_style)];
                    line_spans.extend(spans);
                    return Some(vec![Line::from(line_spans)]);
                } else {
                    let lines = render_tool_result_lines(
                        seg,
                        tool_args,
                        tool_durations,
                        tool_display_registry,
                        expand,
                        render_width,
                        bar_char,
                        bar_style,
                        Some(standalone_label),
                    );
                    return Some(lines);
                }
            }
            None
        }

        _ => None,
    }
}

/// Build a grouped collapsed line for a call+result pair (both tier 0).
fn build_grouped_rich_line(
    tool_name: &str,
    args: &serde_json::Value,
    result_seg: &ChatSegment,
    _tool_args: &std::collections::HashMap<String, String>,
    tool_durations: &std::collections::HashMap<String, f32>,
    display: Option<&dyn sven_tools::ToolDisplay>,
    _tool_display_registry: ToolDisplayRegistryRef,
) -> Vec<Span<'static>> {
    use crate::ui::theme::BAR_ERROR;
    let duration = if let ChatSegment::Message(m) = result_seg {
        if let MessageContent::ToolResult { tool_call_id, .. } = &m.content {
            tool_durations.get(tool_call_id.as_str()).copied()
        } else {
            None
        }
    } else {
        None
    };

    let mut spans = tool_renderer::render_tool_call_collapsed(
        tool_name, args, None, // duration shown on result side
        display, 60,
    );

    // Append result status.
    let (is_error, _tool_call_id) = if let ChatSegment::Message(m) = result_seg {
        if let MessageContent::ToolResult {
            tool_call_id,
            content,
        } = &m.content
        {
            (
                content.to_string().starts_with("error:"),
                tool_call_id.clone(),
            )
        } else {
            (false, String::new())
        }
    } else {
        (false, String::new())
    };

    let status_sym = if is_error { " ✗" } else { " ✓" };
    let status_color = if is_error {
        BAR_ERROR
    } else {
        ratatui::style::Color::Rgb(80, 200, 120)
    };
    let dur_str = if let Some(d) = duration {
        format!("  {:.1}s", d)
    } else {
        String::new()
    };
    spans.push(Span::styled(status_sym, Style::default().fg(status_color)));
    spans.push(Span::styled(
        dur_str,
        Style::default().fg(ratatui::style::Color::Rgb(120, 120, 140)),
    ));
    spans.push(Span::raw("  ▶"));
    spans
}

/// Render a tool result segment as styled lines with bar prefix.
#[allow(clippy::too_many_arguments)]
fn render_tool_result_lines(
    seg: &ChatSegment,
    tool_args: &std::collections::HashMap<String, String>,
    tool_durations: &std::collections::HashMap<String, f32>,
    tool_display_registry: ToolDisplayRegistryRef,
    expand: u8,
    render_width: u16,
    bar_char: &str,
    bar_style: Style,
    label_override: Option<String>,
) -> crate::markdown::StyledLines {
    if let ChatSegment::Message(m) = seg {
        if let MessageContent::ToolResult {
            tool_call_id,
            content,
        } = &m.content
        {
            let tool_name = tool_args
                .get(tool_call_id.as_str())
                .map(|s| s.as_str())
                .unwrap_or("tool");
            let registry_guard = tool_display_registry.as_ref().and_then(|r| r.read().ok());
            let display = registry_guard.as_ref().and_then(|g| g.get(tool_name));
            let output_str = content.to_string();
            let is_error = output_str.starts_with("error:");
            let duration = tool_durations.get(tool_call_id.as_str()).copied();

            // Header: ✓/✗ ToolName  duration
            let result_spans = {
                tool_renderer::render_tool_result_collapsed(
                    tool_name,
                    is_error,
                    duration,
                    display,
                    label_override,
                )
            };
            let mut header_spans = vec![Span::styled(bar_char.to_string(), bar_style)];
            header_spans.extend(result_spans);
            let mut lines: crate::markdown::StyledLines = vec![Line::from(header_spans)];

            if expand >= 1 {
                // Show output body.
                let mut body = tool_renderer::render_tool_result_expanded(
                    tool_name,
                    &output_str,
                    is_error,
                    render_width,
                    display,
                );
                // Skip the status header (already rendered above).
                if !body.is_empty() {
                    body.remove(0);
                }
                for l in body {
                    let mut spans = vec![Span::styled(bar_char.to_string(), bar_style)];
                    spans.extend(l.spans);
                    lines.push(Line::from(spans));
                }
            }
            return lines;
        }
    }
    vec![]
}

/// Get the accent colour for a tool from its display entry or name.
fn tool_renderer_accent(
    display: Option<&dyn sven_tools::ToolDisplay>,
    tool_name: &str,
) -> ratatui::style::Color {
    let category = display
        .map(|d| d.category().to_string())
        .unwrap_or_else(|| sven_tools::tool_category(tool_name).to_string());
    match category.as_str() {
        "file" => ratatui::style::Color::Rgb(100, 180, 255),
        "shell" => ratatui::style::Color::Rgb(120, 220, 130),
        "search" => ratatui::style::Color::Rgb(180, 140, 255),
        "web" => ratatui::style::Color::Rgb(80, 200, 220),
        "system" => ratatui::style::Color::Rgb(200, 160, 60),
        "agent" => ratatui::style::Color::Rgb(220, 120, 180),
        _ => crate::ui::theme::BAR_TOOL,
    }
}
