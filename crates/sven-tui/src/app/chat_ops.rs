// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat display rendering, scroll management, and segment synchronisation helpers.

use ratatui::style::{Color, Style};
use sven_model::MessageContent;
use tracing::debug;

use crate::{
    app::App,
    chat::{
        markdown::{
            apply_bar_and_dim, collapsed_preview, format_conversation, parse_markdown_to_messages,
            segment_bar_style, segment_to_markdown,
        },
        segment::{
            segment_at_line, segment_editable_text, segment_is_removable, segment_is_rerunnable,
            ChatSegment,
        },
    },
    markdown::render_markdown,
};

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
        let mut line_start = 0usize;
        let ascii = self.ascii();
        let bar_char = if ascii { "| " } else { "▌ " };
        let bar_cols: u16 = unicode_width::UnicodeWidthStr::width_cjk(bar_char) as u16;
        let label_reserve: u16 = if self.nvim.disabled { 7 } else { 0 };
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

        for (i, seg) in self.chat.segments.iter().enumerate() {
            let collapsed = self.nvim.disabled && self.chat.collapsed.contains(&i);
            let s = if collapsed {
                collapsed_preview(seg, &self.chat.tool_args)
            } else {
                segment_to_markdown(seg, &self.chat.tool_args)
            };
            let lines = render_markdown(&s, render_width, ascii);
            let (bar_style, dim) = segment_bar_style(seg);
            let styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);
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
            }

            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;
        }

        if !self.chat.streaming_buffer.is_empty() {
            let (s, bar_color) = if self.chat.streaming_is_thinking {
                let prefix = if self.chat.segments.is_empty() {
                    "💭 **Thinking…**\n"
                } else {
                    "\n💭 **Thinking…**\n"
                };
                (
                    format!("{}{}", prefix, self.chat.streaming_buffer),
                    Some(Style::default().fg(Color::Magenta)),
                )
            } else {
                let prefix = if self.chat.segments.is_empty() {
                    "**Agent:** "
                } else {
                    "\n**Agent:** "
                };
                (
                    format!("{}{}", prefix, self.chat.streaming_buffer),
                    Some(Style::default().fg(Color::Blue)),
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
}
