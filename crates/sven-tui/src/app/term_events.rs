// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Terminal event handler: keyboard, mouse, and resize dispatch.

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::layout::Rect;
use sven_model::{MessageContent, Role};

use crate::{
    app::{App, FocusPane},
    chat::segment::{segment_at_line, segment_editable_text, ChatSegment},
    input::{is_reserved_key, to_nvim_notation},
    keys::{map_key, Action},
    layout::AppLayout,
};

use super::dispatch::prev_char_boundary;

impl App {
    // ── Terminal event handler ────────────────────────────────────────────────

    pub(crate) async fn handle_term_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if self.show_help {
                    self.show_help = false;
                    return false;
                }
                if self.question_modal.is_some() {
                    return self.handle_modal_key(k);
                }
                if self.pager.is_some() {
                    return self.handle_pager_key(k).await;
                }

                let in_search = self.search.active;
                let in_input  = self.focus == FocusPane::Input;
                let in_queue  = self.focus == FocusPane::Queue;

                if self.focus == FocusPane::Chat
                    && !in_search
                    && !self.pending_nav
                    && self.nvim_bridge.is_some()
                    && !is_reserved_key(&k)
                {
                    if let Some(nvim_key) = to_nvim_notation(&k) {
                        if let Some(nvim_bridge) = &self.nvim_bridge {
                            let mut bridge = nvim_bridge.lock().await;
                            if let Err(e) = bridge.send_input(&nvim_key).await {
                                tracing::error!("Failed to send key to Neovim: {}", e);
                            }
                        }
                        return false;
                    }
                }

                // When the completion overlay is visible and the input pane
                // has focus, intercept navigation and accept/dismiss keys
                // before they reach the normal input handlers.
                if self.completion_overlay.is_some()
                    && in_input
                    && !in_search
                    && !self.pending_nav
                {
                    use crossterm::event::KeyCode;
                    let shift = k.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                    let overlay_action = match k.code {
                        KeyCode::Enter => Some(Action::CompletionSelect),
                        KeyCode::Esc   => Some(Action::CompletionCancel),
                        KeyCode::Down  => Some(Action::CompletionNext),
                        KeyCode::Up    => Some(Action::CompletionPrev),
                        KeyCode::Tab if !shift => Some(Action::CompletionNext),
                        KeyCode::BackTab       => Some(Action::CompletionPrev),
                        _ => None,
                    };
                    if let Some(action) = overlay_action {
                        self.pending_nav = false;
                        return self.dispatch(action).await;
                    }
                }

                let in_edit_mode = self.editing_message_index.is_some()
                    || self.editing_queue_index.is_some();
                if let Some(action) =
                    map_key(k, in_search, in_input, self.pending_nav, in_edit_mode, in_queue)
                {
                    if action == Action::NavPrefix {
                        self.pending_nav = true;
                        return false;
                    }
                    self.pending_nav = false;
                    return self.dispatch(action).await;
                }
                self.pending_nav = false;
                false
            }

            Event::Mouse(mouse) => {
                if self.pager.is_none() {
                    let over_input = mouse.row >= self.last_input_pane.y
                        && mouse.row < self.last_input_pane.y + self.last_input_pane.height;
                    let over_queue = self.last_queue_pane.height > 0
                        && mouse.row >= self.last_queue_pane.y
                        && mouse.row < self.last_queue_pane.y + self.last_queue_pane.height;
                    let in_edit = self.editing_message_index.is_some()
                        || self.editing_queue_index.is_some();
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if over_input {
                                if in_edit {
                                    self.edit_scroll_offset =
                                        self.edit_scroll_offset.saturating_sub(3);
                                } else {
                                    self.input_scroll_offset =
                                        self.input_scroll_offset.saturating_sub(3);
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-y><C-y><C-y>").await;
                                }
                            } else {
                                self.scroll_up(3);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if over_input {
                                let w = self.last_input_inner_width as usize;
                                let h = self.last_input_inner_height as usize;
                                if w > 0 && h > 0 {
                                    let total = crate::input_wrap::wrap_content(
                                        if in_edit {
                                            &self.edit_buffer
                                        } else {
                                            &self.input_buffer
                                        },
                                        w,
                                        0,
                                    )
                                    .lines
                                    .len();
                                    let max = total.saturating_sub(h);
                                    if in_edit {
                                        self.edit_scroll_offset =
                                            (self.edit_scroll_offset + 3).min(max);
                                    } else {
                                        self.input_scroll_offset =
                                            (self.input_scroll_offset + 3).min(max);
                                    }
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-e><C-e><C-e>").await;
                                }
                            } else {
                                self.scroll_down(3);
                            }
                        }
                        MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            if self.no_nvim =>
                        {
                            // ── Click on queue panel ──────────────────────────────
                            if over_queue && !self.queued.is_empty() {
                                let inner_y = self.last_queue_pane.y + 1; // skip border
                                if mouse.row >= inner_y {
                                    let item_idx = (mouse.row - inner_y) as usize;
                                    if item_idx < self.queued.len() {
                                        self.queue_selected = Some(item_idx);
                                        self.focus = FocusPane::Queue;
                                        if let Some(qm) = self.queued.get(item_idx) {
                                            let text = qm.content.clone();
                                            self.editing_queue_index = Some(item_idx);
                                            self.edit_cursor = text.len();
                                            self.edit_original_text = Some(text.clone());
                                            self.edit_buffer = text;
                                            self.focus = FocusPane::Input;
                                        }
                                    }
                                }
                            }

                            // ── Click on chat pane ───────────────────────────────
                            let content_start_row: u16 = 2;
                            if mouse.row >= content_start_row && !over_queue && !over_input {
                                let click_line = (mouse.row - content_start_row) as usize
                                    + self.scroll_offset as usize;
                                if let Some(seg_idx) =
                                    segment_at_line(&self.segment_line_ranges, click_line)
                                {
                                    if let Some(seg) = self.chat_segments.get(seg_idx) {
                                        let is_editable =
                                            segment_editable_text(&self.chat_segments, seg_idx)
                                                .is_some();
                                        if is_editable {
                                            if let Some(text) = segment_editable_text(
                                                &self.chat_segments,
                                                seg_idx,
                                            ) {
                                                self.editing_message_index = Some(seg_idx);
                                                self.edit_cursor = text.len();
                                                self.edit_original_text = Some(text.clone());
                                                self.edit_buffer = text;
                                                self.focus = FocusPane::Input;
                                                self.update_editing_segment_live();
                                                self.rerender_chat().await;
                                            }
                                        } else {
                                            let is_collapsible = match seg {
                                                ChatSegment::Message(m) => matches!(
                                                    (&m.role, &m.content),
                                                    (Role::Assistant, MessageContent::ToolCall { .. })
                                                        | (
                                                            Role::Tool,
                                                            MessageContent::ToolResult { .. },
                                                        )
                                                ),
                                                ChatSegment::Thinking { .. } => true,
                                                _ => false,
                                            };
                                            if is_collapsible {
                                                if self.collapsed_segments.contains(&seg_idx) {
                                                    self.collapsed_segments.remove(&seg_idx);
                                                } else {
                                                    self.collapsed_segments.insert(seg_idx);
                                                }
                                                self.build_display_from_segments();
                                                self.search.update_matches(&self.chat_lines);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                false
            }

            Event::Resize(width, height) => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let layout = AppLayout::compute(
                        Rect::new(0, 0, width, height),
                        self.search.active,
                        self.queued.len(),
                    );
                    let chat_width  = layout.chat_pane.width.saturating_sub(2);
                    let chat_height = layout.chat_inner_height();
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.resize(chat_width, chat_height).await {
                        tracing::error!("Failed to resize Neovim UI: {}", e);
                    }
                }
                self.rerender_chat().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                false
            }

            _ => false,
        }
    }

    // ── Question modal key handling ───────────────────────────────────────────

    pub(crate) fn handle_modal_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        let modal = match &mut self.question_modal {
            Some(m) => m,
            None => return false,
        };

        match k.code {
            KeyCode::Esc => {
                let modal = self.question_modal.take().unwrap();
                modal.cancel();
            }
            KeyCode::Enter => {
                let done = modal.submit();
                if done {
                    let modal = self.question_modal.take().unwrap();
                    modal.finish();
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                modal.toggle_other();
            }
            KeyCode::Char(c @ '1'..='9') => {
                // Option number (1-indexed from display, 0-indexed internally)
                if let Some(idx) = c.to_digit(10) {
                    let option_idx = idx as usize - 1;
                    if modal.current_q < modal.questions.len() {
                        let q = &modal.questions[modal.current_q];
                        if option_idx < q.options.len() {
                            modal.toggle_option(option_idx);
                        }
                    }
                }
            }
            // Text input for "Other" field when Other is selected
            KeyCode::Char(c) if modal.other_selected => {
                modal.other_input.insert(modal.other_cursor, c);
                modal.other_cursor += c.len_utf8();
            }
            KeyCode::Backspace if modal.other_selected => {
                if modal.other_cursor > 0 {
                    let prev = prev_char_boundary(&modal.other_input, modal.other_cursor);
                    modal.other_input.remove(prev);
                    modal.other_cursor = prev;
                }
            }
            KeyCode::Left if modal.other_selected => {
                modal.other_cursor =
                    prev_char_boundary(&modal.other_input, modal.other_cursor);
            }
            KeyCode::Right if modal.other_selected => {
                if modal.other_cursor < modal.other_input.len() {
                    let ch = modal.other_input[modal.other_cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    modal.other_cursor += ch;
                }
            }
            KeyCode::Home if modal.other_selected => {
                modal.other_cursor = 0;
            }
            KeyCode::End if modal.other_selected => {
                modal.other_cursor = modal.other_input.len();
            }
            _ => {}
        }
        false
    }

    // ── Pager key handling ────────────────────────────────────────────────────

    pub(crate) async fn handle_pager_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crate::keys::map_search_key;
        use crate::pager::PagerAction;

        if self.search.active {
            if let Some(action) = map_search_key(k) {
                return self.dispatch(action).await;
            }
            return false;
        }

        let pager = match &mut self.pager {
            Some(p) => p,
            None => return false,
        };

        match pager.handle_key(k) {
            PagerAction::Close => {
                self.pager = None;
            }
            PagerAction::OpenSearch => {
                self.search.query.clear();
                self.search.current = 0;
                self.search.update_matches(&self.chat_lines);
                self.search.active = true;
            }
            PagerAction::SearchNext => {
                if !self.search.matches.is_empty() {
                    self.search.current =
                        (self.search.current + 1) % self.search.matches.len();
                    if let Some(line) = self.search.current_line() {
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::SearchPrev => {
                if !self.search.matches.is_empty() {
                    self.search.current = self.search.current
                        .checked_sub(1)
                        .unwrap_or(self.search.matches.len() - 1);
                    if let Some(line) = self.search.current_line() {
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::Handled => {}
        }
        false
    }
}
