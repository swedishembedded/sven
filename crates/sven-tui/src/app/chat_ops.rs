// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Chat display rendering, scroll management, and segment synchronisation helpers.

use ratatui::style::{Color, Style};
use sven_model::MessageContent;
use tracing::debug;

use crate::{
    app::App,
    chat::{
        markdown::{
            apply_bar_and_dim, collapsed_preview, format_conversation,
            parse_markdown_to_messages, segment_bar_style, segment_to_markdown,
        },
        segment::ChatSegment,
    },
    markdown::render_markdown,
};

impl App {
    // ── Chat display ──────────────────────────────────────────────────────────

    /// Rebuild `chat_lines` and `segment_line_ranges` from `chat_segments` plus
    /// the streaming buffer.
    pub(crate) fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges    = Vec::new();
        let mut line_start = 0usize;
        let ascii = self.ascii();
        let bar_char = if ascii { "| " } else { "▌ " };

        let bar_cols: u16 = 2;
        let effective_width = self.last_chat_inner_width.saturating_sub(bar_cols).max(20);
        let render_width = if self.config.tui.wrap_width == 0 {
            effective_width
        } else {
            self.config.tui.wrap_width.min(effective_width)
        };

        for (i, seg) in self.chat_segments.iter().enumerate() {
            let s = if self.no_nvim && self.collapsed_segments.contains(&i) {
                collapsed_preview(seg, &self.tool_args_cache)
            } else {
                segment_to_markdown(seg, &self.tool_args_cache)
            };
            let lines = render_markdown(&s, render_width, ascii);
            let (bar_style, dim) = segment_bar_style(seg);
            let styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);
            let n = styled.len();
            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;
        }
        if !self.streaming_assistant_buffer.is_empty() {
            let (s, bar_color) = if self.streaming_is_thinking {
                let prefix = if self.chat_segments.is_empty() { "💭 **Thinking…**\n" } else { "\n💭 **Thinking…**\n" };
                (
                    format!("{}{}", prefix, self.streaming_assistant_buffer),
                    Some(Style::default().fg(Color::Magenta)),
                )
            } else {
                let prefix = if self.chat_segments.is_empty() { "**Agent:** " } else { "\n**Agent:** " };
                (
                    format!("{}{}", prefix, self.streaming_assistant_buffer),
                    Some(Style::default().fg(Color::Blue)),
                )
            };
            let lines = render_markdown(&s, render_width, ascii);
            let styled = apply_bar_and_dim(lines, bar_color, false, bar_char);
            all_lines.extend(styled);
        }
        self.chat_lines = all_lines;
        self.segment_line_ranges = ranges;
    }

    /// Re-render the chat pane: update the Neovim buffer (if active) and
    /// rebuild the ratatui display lines.
    pub(crate) async fn rerender_chat(&mut self) {
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let content = format_conversation(
                &self.chat_segments,
                &self.streaming_assistant_buffer,
                &self.tool_args_cache,
            );
            let mut bridge = nvim_bridge.lock().await;
            if let Err(e) = bridge.set_modifiable(true).await {
                tracing::error!("Failed to set buffer modifiable for update: {}", e);
            }
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
            if self.agent_busy {
                if let Err(e) = bridge.set_modifiable(false).await {
                    tracing::error!("Failed to set buffer non-modifiable: {}", e);
                }
            }
        }
        self.build_display_from_segments();
        self.search.update_matches(&self.chat_lines);
    }

    pub(crate) fn ascii(&self) -> bool {
        if std::env::var("SVEN_ASCII_BORDERS").as_deref() == Ok("1") {
            return true;
        }
        self.config.tui.ascii_borders
    }

    // ── Scroll helpers ────────────────────────────────────────────────────────

    pub(crate) fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.auto_scroll = false;
    }

    pub(crate) fn scroll_down(&mut self, n: u16) {
        let max = (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        self.scroll_offset = (self.scroll_offset + n).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        if self.nvim_bridge.is_none() && self.auto_scroll {
            self.scroll_offset =
                (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        }
    }

    /// Adjust `input_scroll_offset` so the cursor row is within the visible
    /// window of the input pane.
    pub(crate) fn adjust_input_scroll(&mut self) {
        let w = self.last_input_inner_width as usize;
        let h = self.last_input_inner_height as usize;
        if w == 0 || h == 0 { return; }
        let wrap = crate::input_wrap::wrap_content(&self.input_buffer, w, self.input_cursor);
        crate::input_wrap::adjust_scroll(wrap.cursor_row, h, &mut self.input_scroll_offset);
    }

    /// Adjust `edit_scroll_offset` so the cursor row is within the visible
    /// window when in inline edit mode.
    pub(crate) fn adjust_edit_scroll(&mut self) {
        let w = self.last_input_inner_width as usize;
        let h = self.last_input_inner_height as usize;
        if w == 0 || h == 0 { return; }
        let wrap = crate::input_wrap::wrap_content(&self.edit_buffer, w, self.edit_cursor);
        crate::input_wrap::adjust_scroll(wrap.cursor_row, h, &mut self.edit_scroll_offset);
    }

    // ── History persistence ───────────────────────────────────────────────────

    /// Persist the conversation to disk asynchronously.
    pub(crate) fn save_history_async(&mut self) {
        let records: Vec<sven_input::ConversationRecord> = self
            .chat_segments
            .iter()
            .filter_map(|seg| match seg {
                ChatSegment::Message(m) => {
                    Some(sven_input::ConversationRecord::Message(m.clone()))
                }
                ChatSegment::Thinking { content } => {
                    Some(sven_input::ConversationRecord::Thinking { content: content.clone() })
                }
                ChatSegment::ContextCompacted { tokens_before, tokens_after, strategy, turn } => {
                    Some(sven_input::ConversationRecord::ContextCompacted {
                        tokens_before: *tokens_before,
                        tokens_after: *tokens_after,
                        strategy: Some(strategy.to_string()),
                        turn: Some(*turn),
                    })
                }
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
            None => {
                match sven_input::history::save(&messages) {
                    Ok(path) => {
                        debug!(path = %path.display(), "conversation saved to history");
                        self.history_path = Some(path);
                    }
                    Err(e) => debug!("failed to save conversation to history: {e}"),
                }
            }
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

    /// Read the Neovim buffer and update `chat_segments` from its current
    /// content.  Called before submitting so in-buffer edits are preserved.
    pub(crate) async fn sync_nvim_buffer_to_segments(&mut self) {
        let content = if let Some(nvim_bridge) = &self.nvim_bridge {
            let bridge = nvim_bridge.lock().await;
            bridge.get_buffer_content().await.ok()
        } else {
            return;
        };
        if let Some(content) = content {
            match parse_markdown_to_messages(&content) {
                Ok(messages) if !messages.is_empty() => {
                    self.chat_segments = messages
                        .iter()
                        .map(|m| ChatSegment::Message(m.clone()))
                        .collect();
                    self.tool_args_cache.clear();
                    for m in &messages {
                        if let MessageContent::ToolCall { tool_call_id, function } = &m.content {
                            self.tool_args_cache.insert(tool_call_id.clone(), function.name.clone());
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
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let mut bridge = nvim_bridge.lock().await;
            let _ = bridge.send_input("G").await;
        }
    }
}
