// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Terminal event handler: keyboard, mouse, and resize dispatch.

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::layout::Rect;
use sven_model::{MessageContent, Role};

use crate::{
    app::{App, FocusPane},
    chat::segment::{segment_at_line, segment_editable_text, segment_short_preview, ChatSegment},
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
                if self.confirm_modal.is_some() {
                    return self.handle_confirm_modal_key(k).await;
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
                    let ctrl  = k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                    let alt   = k.modifiers.contains(crossterm::event::KeyModifiers::ALT);
                    let overlay_action = match k.code {
                        // Plain Enter accepts the highlighted completion.
                        // Shift/Ctrl/Alt+Enter inserts a newline instead of accepting — let
                        // the action fall through to the regular key handler below.
                        KeyCode::Enter if !shift && !ctrl && !alt => Some(Action::CompletionSelect),
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
                                        self.edit_scroll_offset.saturating_sub(1);
                                } else {
                                    self.input_scroll_offset =
                                        self.input_scroll_offset.saturating_sub(1);
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-y>").await;
                                }
                            } else {
                                self.scroll_up(1);
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
                                            (self.edit_scroll_offset + 1).min(max);
                                    } else {
                                        self.input_scroll_offset =
                                            (self.input_scroll_offset + 1).min(max);
                                    }
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-e>").await;
                                }
                            } else {
                                self.scroll_down(1);
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
                            // The chat pane inner area starts at row 1 (border)
                            // and column last_chat_pane.x + 1 (border).
                            let chat_inner_x = self.last_chat_pane.x + 1;
                            let chat_inner_w = self.last_chat_pane.width.saturating_sub(2);
                            let chat_inner_h = self.last_chat_pane.height.saturating_sub(2);
                            let content_start_row = self.last_chat_pane.y + 1;

                            // ── Scrollbar click ──────────────────────────────────
                            // The scrollbar occupies the rightmost column of the
                            // inner chat area.  Clicking it scrolls proportionally.
                            let scrollbar_col = chat_inner_x + chat_inner_w.saturating_sub(1);
                            let total_chat_lines = self.chat_lines.len() as u16;
                            let over_chat = mouse.row >= content_start_row
                                && mouse.row < content_start_row + chat_inner_h
                                && !over_queue
                                && !over_input;
                            if over_chat
                                && mouse.column == scrollbar_col
                                && chat_inner_h > 0
                                && total_chat_lines > chat_inner_h
                            {
                                let rel_row = mouse.row - content_start_row;
                                let new_offset = (rel_row as u32
                                    * (total_chat_lines - chat_inner_h) as u32
                                    / chat_inner_h.saturating_sub(1).max(1) as u32)
                                    as u16;
                                self.scroll_offset =
                                    new_offset.min(total_chat_lines.saturating_sub(chat_inner_h));
                                self.auto_scroll = false;
                                self.recompute_focused_segment();
                            }

                            // Skip segment-click logic when the click landed on the scrollbar.
                            let clicked_scrollbar = mouse.column == scrollbar_col
                                && total_chat_lines > chat_inner_h;
                            if mouse.row >= content_start_row
                                && !over_queue
                                && !over_input
                                && !clicked_scrollbar
                            {
                                let click_line = (mouse.row - content_start_row) as usize
                                    + self.scroll_offset as usize;
                                if let Some(seg_idx) =
                                    segment_at_line(&self.segment_line_ranges, click_line)
                                {
                                    let _is_editable =
                                        segment_editable_text(&self.chat_segments, seg_idx)
                                            .is_some();

                                    // Detect clicks on the right-aligned action icons.
                                    // Layout (6 cols from inner right edge, 1-col margin at w-1):
                                    //   ↻ rerun  — col w-6  (zone [w-6, w-5])
                                    //   ✎ edit   — col w-4  (zone [w-5, w-4])
                                    //   ✕ delete — col w-2  (zone [w-3, w-2])
                                    let is_header_line =
                                        self.remove_label_line_indices.contains(&click_line);

                                    // Zone boundaries (cols, right-edge relative, non-overlapping)
                                    let label_area_start  = chat_inner_x + chat_inner_w.saturating_sub(6);
                                    let edit_zone_start   = chat_inner_x + chat_inner_w.saturating_sub(5);
                                    let delete_zone_start = chat_inner_x + chat_inner_w.saturating_sub(3);

                                    let clicked_delete = is_header_line
                                        && mouse.column >= delete_zone_start
                                        && mouse.column < chat_inner_x + chat_inner_w.saturating_sub(1);
                                    let clicked_edit = is_header_line
                                        && self.edit_label_line_indices.contains(&click_line)
                                        && mouse.column >= edit_zone_start
                                        && mouse.column < delete_zone_start;
                                    let clicked_rerun = is_header_line
                                        && self.rerun_label_line_indices.contains(&click_line)
                                        && mouse.column >= label_area_start
                                        && mouse.column < edit_zone_start;
                                    // Any click outside the label area cancels a pending delete.
                                    let outside_labels = mouse.column < label_area_start;

                                    if clicked_delete {
                                        // Open the confirmation modal.
                                        use crate::overlay::confirm::{ConfirmModal, ConfirmedAction};
                                        let preview = segment_short_preview(
                                            self.chat_segments.get(seg_idx),
                                        );
                                        self.confirm_modal = Some(ConfirmModal::new(
                                            "Delete message",
                                            &format!("Remove this message from the conversation?\n{preview}"),
                                            ConfirmedAction::RemoveSegment(seg_idx),
                                        ));
                                    } else if clicked_edit {
                                        // Load segment into the edit buffer.
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
                                    } else if clicked_rerun {
                                        self.confirm_modal = None;
                                        let saved = self.focused_chat_segment;
                                        self.focused_chat_segment = Some(seg_idx);
                                        self.dispatch(Action::RerunFromSegment).await;
                                        if self.focused_chat_segment.is_some() {
                                            self.focused_chat_segment = saved;
                                        }
                                    } else {
                                        if outside_labels {
                                            self.confirm_modal = None;
                                        }
                                        // All other clicks on any segment: toggle collapse.
                                        let is_collapsible = match self.chat_segments.get(seg_idx) {
                                            Some(ChatSegment::Message(m)) => matches!(
                                                (&m.role, &m.content),
                                                (Role::User, MessageContent::Text(_))
                                                    | (Role::Assistant, MessageContent::Text(_))
                                                    | (
                                                        Role::Assistant,
                                                        MessageContent::ToolCall { .. },
                                                    )
                                                    | (
                                                        Role::Tool,
                                                        MessageContent::ToolResult { .. },
                                                    )
                                            ),
                                            Some(ChatSegment::Thinking { .. }) => true,
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
                                            // Clamp scroll so collapsing content never leaves a
                                            // blank viewport, then ensure the toggled segment
                                            // remains visible.
                                            let max_offset = (self.chat_lines.len() as u16)
                                                .saturating_sub(self.chat_height);
                                            self.scroll_offset =
                                                self.scroll_offset.min(max_offset);
                                            // If the collapsed segment now starts below the
                                            // viewport, scroll up to bring it into view.
                                            if let Some(&(seg_start, _)) =
                                                self.segment_line_ranges.get(seg_idx)
                                            {
                                                if (seg_start as u16) < self.scroll_offset {
                                                    self.scroll_offset = seg_start as u16;
                                                }
                                            }
                                            self.recompute_focused_segment();
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
            // ── Cancel ────────────────────────────────────────────────────────
            // Esc outside text mode: cancel the whole modal.
            KeyCode::Esc if !modal.other_selected => {
                let modal = self.question_modal.take().unwrap();
                modal.cancel();
            }
            // Esc inside Other text mode: cancel the edit and restore the
            // snapshot (text typed before this edit session is kept).
            KeyCode::Esc if modal.other_selected => {
                modal.cancel_other_edit();
            }

            // ── Submit / advance / activate text mode ─────────────────────────
            // Enter outside text mode:
            //   • Other row focused, no text yet   → enter text-edit mode
            //   • Other row focused, has text       → submit
            //   • Regular row, nothing selected yet → auto-select it, then submit
            //   • Regular row, something selected   → submit as-is
            KeyCode::Enter if !modal.other_selected => {
                let n_opts = modal.questions
                    .get(modal.current_q)
                    .map(|q| q.options.len())
                    .unwrap_or(0);
                if modal.focused_option == n_opts && !modal.other_has_text() {
                    // Other row focused but empty: enter text-edit mode.
                    modal.activate_other();
                } else {
                    // For regular options: if nothing is selected yet, select the
                    // focused option so Enter without a prior Space still works.
                    if modal.focused_option < n_opts && modal.selected_options.is_empty() {
                        modal.toggle_option(modal.focused_option);
                    }
                    let done = modal.submit();
                    if done {
                        let modal = self.question_modal.take().unwrap();
                        modal.finish();
                    }
                }
            }
            // Enter inside text mode: accept the typed text and exit text mode.
            // The user then presses Enter again to submit the whole answer.
            KeyCode::Enter if modal.other_selected => {
                modal.deactivate_other();
            }

            // ── Arrow-key navigation ─────────────────────────────────────────
            // Up: move focus to the previous row; at the first row go back to
            // the previous question (replaces Shift+Tab).
            KeyCode::Up if !modal.other_selected => {
                if modal.focused_option == 0 {
                    modal.go_back();
                } else {
                    modal.focus_prev();
                }
            }
            KeyCode::Down if !modal.other_selected => {
                modal.focus_next();
            }

            // ── Space: select/toggle focused row ──────────────────────────────
            KeyCode::Char(' ') if !modal.other_selected => {
                modal.select_focused();
            }
            // Space inside text mode: insert a literal space.
            KeyCode::Char(' ') if modal.other_selected => {
                modal.other_input.insert(modal.other_cursor, ' ');
                modal.other_cursor += 1;
            }

            // ── Number shortcut: 1-9 toggles the corresponding option ─────────
            KeyCode::Char(c @ '1'..='9') if !modal.other_selected => {
                if let Some(idx) = c.to_digit(10) {
                    let option_idx = idx as usize - 1;
                    if modal.current_q < modal.questions.len() {
                        let q = &modal.questions[modal.current_q];
                        if option_idx == q.options.len() {
                            modal.activate_other();
                        } else if option_idx < q.options.len() {
                            modal.toggle_option(option_idx);
                        }
                    }
                }
            }

            // ── Text input for the "Other" free-text field ────────────────────
            // All printable characters (including Shift+letter = uppercase).
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
            KeyCode::Delete if modal.other_selected => {
                if modal.other_cursor < modal.other_input.len() {
                    modal.other_input.remove(modal.other_cursor);
                }
            }
            KeyCode::Left if modal.other_selected => {
                if modal.other_cursor > 0 {
                    modal.other_cursor =
                        prev_char_boundary(&modal.other_input, modal.other_cursor);
                }
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

    // ── Confirm-modal key handling ────────────────────────────────────────────

    /// Handle keyboard events when the confirmation modal is open.
    ///
    /// - `←` / `→` / `Tab`: toggle focus between Confirm and Cancel buttons
    /// - `Enter`: activate the focused button (confirm or cancel)
    /// - `Esc`: cancel (same as activating Cancel)
    pub(crate) async fn handle_confirm_modal_key(
        &mut self,
        k: crossterm::event::KeyEvent,
    ) -> bool {
        use crossterm::event::KeyCode;
        use crate::overlay::confirm::ConfirmedAction;

        match k.code {
            KeyCode::Esc => {
                self.confirm_modal = None;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                if let Some(modal) = &mut self.confirm_modal {
                    modal.focus_next();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(modal) = self.confirm_modal.take() {
                    // Focused on Cancel or info-only dialog → just dismiss.
                    if modal.focused_button != 0 || !modal.has_action() {
                        return false;
                    }
                    // Focused on Confirm → execute the stored action.
                    if let Some(action) = modal.action {
                        match action {
                            ConfirmedAction::RemoveSegment(seg_idx) => {
                                let saved = self.focused_chat_segment;
                                self.focused_chat_segment = Some(seg_idx);
                                self.dispatch(Action::RemoveChatSegment).await;
                                if self.focused_chat_segment.is_some() {
                                    self.focused_chat_segment = saved;
                                }
                            }
                        }
                    }
                }
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
