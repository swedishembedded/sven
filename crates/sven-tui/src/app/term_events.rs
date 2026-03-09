// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Terminal event handler: keyboard, mouse, and resize dispatch.
//!
//! Routing contract
//! ────────────────
//! `handle_term_event` translates raw terminal events into [`Action`]s and
//! immediately calls `dispatch`.  It **must not** mutate `App` state directly
//! (except for the three stateful cases listed below).
//!
//! The only direct mutations allowed here are:
//!  1. `self.ui.show_help = false` (single-field clear, no logic)
//!  2. `self.layout.resize_drag` — border-drag state machine that spans
//!     multiple events and cannot be expressed as a single `Action`.
//!  3. `self.ui.pending_nav` — transient key-prefix flag.
//!
//! Everything else goes through `mouse_to_action()` → `dispatch()`.

use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::hit_test::{hit_test, HitArea},
    app::input_state::{is_image_path, InputAttachment},
    app::layout_cache::ResizeDrag,
    app::{App, FocusPane},
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
                if self.ui.show_help {
                    self.ui.show_help = false;
                    return false;
                }
                // Team picker overlay intercepts keys — all mutations route
                // through dispatch() so the logic lives in exactly one place.
                if self.ui.show_team_picker {
                    use crossterm::event::{KeyCode, KeyModifiers};
                    let action = match k.code {
                        KeyCode::Esc | KeyCode::Char('q') => Some(Action::TeamPickerClose),
                        KeyCode::Down | KeyCode::Char('j') => Some(Action::TeamPickerNext),
                        KeyCode::Up | KeyCode::Char('k') => Some(Action::TeamPickerPrev),
                        KeyCode::Enter => Some(Action::TeamPickerSelect),
                        // Ctrl+a again closes.
                        KeyCode::Char('a') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::TeamPickerClose)
                        }
                        _ => None,
                    };
                    if let Some(a) = action {
                        return self.dispatch(a).await;
                    }
                    return false;
                }
                if self.ui.question_modal.is_some() {
                    return self.handle_modal_key(k);
                }
                if self.ui.confirm_modal.is_some() {
                    return self.handle_confirm_modal_key(k).await;
                }
                if self.ui.inspector.is_some() {
                    return self.handle_inspector_key(k).await;
                }
                if self.ui.pager.is_some() {
                    return self.handle_pager_key(k).await;
                }

                let in_search = self.ui.search.active;
                let in_input = self.ui.focus == FocusPane::Input;
                let in_queue = self.ui.focus == FocusPane::Queue;

                if self.ui.focus == FocusPane::Chat
                    && !in_search
                    && !self.ui.pending_nav
                    && self.nvim.bridge.is_some()
                    && !is_reserved_key(&k)
                {
                    if let Some(nvim_key) = to_nvim_notation(&k) {
                        if let Some(nvim_bridge) = &self.nvim.bridge {
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
                if self.ui.completion.is_some() && in_input && !in_search && !self.ui.pending_nav {
                    use crossterm::event::KeyCode;
                    let shift = k.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                    let ctrl = k
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL);
                    let alt = k.modifiers.contains(crossterm::event::KeyModifiers::ALT);
                    let overlay_action = match k.code {
                        // Plain Enter accepts the highlighted completion.
                        // Shift/Ctrl/Alt+Enter inserts a newline instead of accepting.
                        KeyCode::Enter if !shift && !ctrl && !alt => Some(Action::CompletionSelect),
                        KeyCode::Esc => Some(Action::CompletionCancel),
                        KeyCode::Down => Some(Action::CompletionNext),
                        KeyCode::Up => Some(Action::CompletionPrev),
                        KeyCode::Tab if !shift => Some(Action::CompletionNext),
                        KeyCode::BackTab => Some(Action::CompletionPrev),
                        _ => None,
                    };
                    if let Some(action) = overlay_action {
                        self.ui.pending_nav = false;
                        return self.dispatch(action).await;
                    }
                }

                let in_edit_mode =
                    self.edit.message_index.is_some() || self.edit.queue_index.is_some();
                let in_chat_list = self.ui.focus == FocusPane::ChatList;
                if let Some(action) = map_key(
                    k,
                    in_search,
                    in_input,
                    self.ui.pending_nav,
                    in_edit_mode,
                    in_queue,
                    in_chat_list,
                ) {
                    if action == Action::NavPrefix {
                        self.ui.pending_nav = true;
                        return false;
                    }
                    self.ui.pending_nav = false;
                    return self.dispatch(action).await;
                }
                self.ui.pending_nav = false;
                false
            }

            Event::Mouse(mouse) => {
                // ── Overlay intercepts: inspector and pager eat scroll wheel ──
                // These short-circuit before any pane routing.
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_up(3);
                            return false;
                        }
                        if let Some(pager) = &mut self.ui.pager {
                            pager.scroll_up(3);
                            return false;
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_down(3);
                            return false;
                        }
                        if let Some(pager) = &mut self.ui.pager {
                            pager.scroll_down(3);
                            return false;
                        }
                    }
                    _ => {}
                }

                // ── Pane border resize (stateful drag; spans multiple events) ─
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if self.layout.on_chat_list_border(mouse.column, mouse.row) {
                            self.layout.resize_drag = Some(ResizeDrag::ChatListWidth);
                            return false;
                        }
                        if self.layout.on_input_border(mouse.column, mouse.row) {
                            self.layout.resize_drag = Some(ResizeDrag::InputHeight);
                            return false;
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => match self.layout.resize_drag {
                        Some(ResizeDrag::ChatListWidth) => {
                            self.layout.drag_chat_list_width(mouse.column);
                            return false;
                        }
                        Some(ResizeDrag::InputHeight) => {
                            self.layout.drag_input_height(mouse.row);
                            return false;
                        }
                        None => {}
                    },
                    MouseEventKind::Up(MouseButton::Left) => {
                        if self.layout.resize_drag.is_some() {
                            self.layout.resize_drag = None;
                            return false;
                        }
                    }
                    _ => {}
                }

                // ── Route remaining events through mouse_to_action → dispatch ─
                if let Some(action) = self.mouse_to_action(mouse) {
                    return self.dispatch(action).await;
                }
                false
            }

            Event::Resize(width, height) => {
                // Estimate dynamic input height for the layout cache update.
                let prompt_width: u16 = 2;
                let avail_wrap_width = width.saturating_sub(prompt_width).max(1) as usize;
                let wrap_est = crate::input_wrap::wrap_content(
                    &self.input.buffer,
                    avail_wrap_width,
                    self.input.buffer.len(),
                );
                let text_lines = wrap_est.lines.len().max(1) as u16;
                let attach_rows = self.input.attachments.len() as u16;
                let max_input_height = (height / 2).max(3);
                let desired_input_height = (text_lines + attach_rows + 2)
                    .max(self.layout.input_height_pref)
                    .min(max_input_height);
                let layout = AppLayout::compute(
                    Rect::new(0, 0, width, height),
                    self.ui.search.active,
                    self.queue.messages.len(),
                    desired_input_height,
                    self.layout.effective_chat_list_width(),
                );
                // Open-border panes (TOP+BOTTOM only) — no left/right `│` chars.
                self.layout.chat_inner_width = layout.chat_pane.width.max(20);
                self.layout.chat_height = layout.chat_inner_height().max(1);
                // Input pane: no left/right borders; reserve 2 cols for `> ` prompt.
                self.layout.input_inner_width = layout.input_pane.width.saturating_sub(2);
                self.layout.input_inner_height = layout.input_pane.height.saturating_sub(2);
                self.layout.chat_pane = layout.chat_pane;
                self.layout.input_pane = layout.input_pane;
                self.layout.queue_pane = layout.queue_pane;
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let chat_width = layout.chat_pane.width.saturating_sub(2);
                    let chat_height = layout.chat_inner_height();
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.resize(chat_width, chat_height).await {
                        tracing::error!("Failed to resize Neovim UI: {}", e);
                    }
                }
                self.rerender_chat().await;
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
                false
            }

            // ── Bracketed paste ───────────────────────────────────────────────
            Event::Paste(text) => {
                // Normalise line endings first.
                let normalised: String = text.replace("\r\n", "\n").replace('\r', "\n");

                // ── Per-line path / image detection ───────────────────────────
                // Only image files (png, jpg, etc.) are attached as context
                // objects; all other paths (directories, code files, text files)
                // are inserted inline so the model sees them as plain text and
                // the user can edit them freely.
                //
                // Multi-line pastes are checked line by line.  A line that
                // resolves as an image becomes an attachment (consumed, not
                // inserted).  Every other line — including resolved non-image
                // paths — is inserted into the buffer as-is.
                let lines: Vec<&str> = normalised.split('\n').collect();
                let single_line = lines.len() == 1;
                let mut any_inserted = false;
                for (idx, line) in lines.iter().enumerate() {
                    let candidate = line.trim();
                    if let Some(path_buf) = Self::resolve_paste_path(candidate) {
                        if is_image_path(&path_buf) {
                            self.input.attachments.push(InputAttachment::new(path_buf));
                            // Don't insert image paths into the buffer.
                            continue;
                        }
                        // Non-image path: insert as inline text.
                    }
                    // Insert the line text.  Add a newline between lines (but
                    // not after the final segment of a multi-line paste).
                    if any_inserted || (!single_line && idx > 0) {
                        self.input.buffer.insert(self.input.cursor, '\n');
                        self.input.cursor += 1;
                    }
                    for ch in line.chars() {
                        self.input.buffer.insert(self.input.cursor, ch);
                        self.input.cursor += ch.len_utf8();
                    }
                    any_inserted = true;
                }
                if self.should_show_completion() {
                    self.update_completion_overlay();
                }
                false
            }

            _ => false,
        }
    }

    // ── Mouse routing ─────────────────────────────────────────────────────────

    /// Translate a raw [`MouseEvent`] into a logical [`Action`].
    ///
    /// This function is **read-only** — it never mutates `App` state.  All
    /// mutations happen in `dispatch` once the action is returned.
    ///
    /// The border-resize drag and overlay scroll are handled before this is
    /// called, so those events never reach here.
    fn mouse_to_action(&self, mouse: MouseEvent) -> Option<Action> {
        // Nothing to route while the pager is open (overlay scroll already
        // handled; everything else should be a no-op in pager mode).
        if self.ui.pager.is_some() {
            return None;
        }

        let area = hit_test(
            &self.layout,
            mouse.column,
            mouse.row,
            self.chat.scroll_offset,
            self.chat.lines.len(),
            self.queue.messages.len(),
        );

        match (mouse.kind, area) {
            // ── Chat list sidebar ─────────────────────────────────────────────
            (MouseEventKind::Down(MouseButton::Left), HitArea::ChatList { inner_row }) => {
                Some(Action::ChatListClick { inner_row })
            }

            // ── Scroll wheel ─────────────────────────────────────────────────
            (MouseEventKind::ScrollUp, HitArea::InputPane) => Some(Action::InputScrollUp),
            (MouseEventKind::ScrollDown, HitArea::InputPane) => Some(Action::InputScrollDown),
            (MouseEventKind::ScrollUp, _) if self.nvim.bridge.is_some() => {
                Some(Action::NvimScrollUp)
            }
            (MouseEventKind::ScrollDown, _) if self.nvim.bridge.is_some() => {
                Some(Action::NvimScrollDown)
            }
            (MouseEventKind::ScrollUp, _) => Some(Action::ScrollUp),
            (MouseEventKind::ScrollDown, _) => Some(Action::ScrollDown),

            // ── Interactions that require ratatui (nvim disabled) ─────────────
            (MouseEventKind::Down(MouseButton::Left), HitArea::ChatScrollbar { rel_row })
                if self.nvim.disabled =>
            {
                Some(Action::ChatScrollbarClick { rel_row })
            }

            (MouseEventKind::Down(MouseButton::Left), HitArea::QueueItem { index })
                if self.nvim.disabled =>
            {
                Some(Action::QueueClick { index })
            }

            (
                MouseEventKind::Down(MouseButton::Left),
                HitArea::ChatContent {
                    abs_line,
                    inner_col,
                },
            ) if self.nvim.disabled => Some(Action::ChatContentClick {
                abs_line,
                inner_col,
            }),

            // Clicks outside chat content clear any active selection.
            (MouseEventKind::Down(MouseButton::Left), _) if self.nvim.disabled => {
                Some(Action::SelectionClear)
            }

            // ── Selection drag ────────────────────────────────────────────────
            // Drag always clamps to the chat content area regardless of where
            // the pointer currently is, so we compute abs_line directly here
            // instead of relying on hit_test's area.
            (MouseEventKind::Drag(MouseButton::Left), _)
                if self.nvim.disabled && self.chat.selection_anchor.is_some() =>
            {
                let cp = self.layout.chat_pane;
                let content_top = cp.y + 1;
                let content_bottom = content_top + cp.height.saturating_sub(2);
                let clamped = mouse
                    .row
                    .clamp(content_top, content_bottom.saturating_sub(1));
                let abs_line = (clamped - content_top) as usize + self.chat.scroll_offset as usize;
                let abs_line = abs_line.min(self.chat.lines.len().saturating_sub(1));
                let inner_col = mouse
                    .column
                    .saturating_sub(cp.x)
                    .min(cp.width.saturating_sub(1));
                Some(Action::SelectionExtend {
                    abs_line,
                    inner_col,
                    mouse_row: mouse.row,
                })
            }

            // ── Selection release ─────────────────────────────────────────────
            (MouseEventKind::Up(MouseButton::Left), _)
                if self.nvim.disabled && self.chat.is_selecting =>
            {
                Some(Action::SelectionFinish)
            }

            _ => None,
        }
    }

    // ── Paste path resolution ─────────────────────────────────────────────────

    /// Try to resolve a paste candidate to an existing filesystem path.
    ///
    /// Handles the following forms (all are tried in order):
    /// - `file:///absolute/path`  (e.g. from file managers)
    /// - `"quoted/path"` or `'quoted/path'`
    /// - `~/relative`
    /// - `./relative`
    /// - absolute paths
    /// - bare filenames / relative paths (resolved against cwd)
    ///
    /// Returns `Some(PathBuf)` only when the resolved path actually **exists**
    /// on the filesystem.
    fn resolve_paste_path(candidate: &str) -> Option<std::path::PathBuf> {
        // Strip file:// URI prefix.
        let s = if let Some(rest) = candidate.strip_prefix("file://") {
            rest
        } else {
            candidate
        };

        // Strip surrounding quotes (single or double).
        let s = s
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(s);

        // Expand leading `~/` → $HOME/.
        let path_buf: std::path::PathBuf = if let Some(rest) = s.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                std::path::PathBuf::from(format!("{home}/{rest}"))
            } else {
                std::path::PathBuf::from(s)
            }
        } else {
            std::path::PathBuf::from(s)
        };

        if path_buf.exists() {
            return Some(path_buf);
        }

        // Try resolving a relative path against the current working directory.
        if path_buf.is_relative() {
            if let Ok(cwd) = std::env::current_dir() {
                let abs = cwd.join(&path_buf);
                if abs.exists() {
                    return Some(abs);
                }
            }
        }

        None
    }

    // ── Question modal key handling ───────────────────────────────────────────

    pub(crate) fn handle_modal_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        let modal = match &mut self.ui.question_modal {
            Some(m) => m,
            None => return false,
        };

        match k.code {
            KeyCode::Esc if !modal.other_selected => {
                let modal = self.ui.question_modal.take().unwrap();
                modal.cancel();
            }
            KeyCode::Esc if modal.other_selected => {
                modal.cancel_other_edit();
            }

            KeyCode::Enter if !modal.other_selected => {
                let n_opts = modal
                    .questions
                    .get(modal.current_q)
                    .map(|q| q.options.len())
                    .unwrap_or(0);
                if modal.focused_option == n_opts && !modal.other_has_text() {
                    modal.activate_other();
                } else {
                    if modal.focused_option < n_opts && modal.selected_options.is_empty() {
                        modal.toggle_option(modal.focused_option);
                    }
                    let done = modal.submit();
                    if done {
                        let modal = self.ui.question_modal.take().unwrap();
                        modal.finish();
                    }
                }
            }
            KeyCode::Enter if modal.other_selected => {
                modal.deactivate_other();
            }

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

            KeyCode::Char(' ') if !modal.other_selected => {
                modal.select_focused();
            }
            KeyCode::Char(' ') if modal.other_selected => {
                modal.other_input.insert(modal.other_cursor, ' ');
                modal.other_cursor += 1;
            }

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
                    modal.other_cursor = prev_char_boundary(&modal.other_input, modal.other_cursor);
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

    pub(crate) async fn handle_confirm_modal_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crate::overlay::confirm::ConfirmedAction;
        use crossterm::event::KeyCode;

        match k.code {
            KeyCode::Esc => {
                self.ui.confirm_modal = None;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                if let Some(modal) = &mut self.ui.confirm_modal {
                    modal.focus_next();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(modal) = self.ui.confirm_modal.take() {
                    if modal.focused_button != 0 || !modal.has_action() {
                        return false;
                    }
                    if let Some(action) = modal.action {
                        match action {
                            ConfirmedAction::RemoveSegment(seg_idx) => {
                                let saved = self.chat.focused_segment;
                                self.chat.focused_segment = Some(seg_idx);
                                self.dispatch(Action::RemoveChatSegment).await;
                                if self.chat.focused_segment.is_some() {
                                    self.chat.focused_segment = saved;
                                }
                            }
                            ConfirmedAction::DeleteChat(id) => {
                                if id == self.sessions.active_id {
                                    let other = self
                                        .sessions
                                        .display_order
                                        .iter()
                                        .find(|x| *x != &id)
                                        .cloned();
                                    if let Some(other_id) = other {
                                        self.switch_session(other_id).await;
                                    } else {
                                        self.new_session().await;
                                    }
                                }
                                if self.sessions.delete(&id) {
                                    self.ui.push_toast(crate::app::ui_state::Toast::info(
                                        "Chat deleted",
                                    ));
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

        if self.ui.search.active {
            if let Some(action) = map_search_key(k) {
                return self.dispatch(action).await;
            }
            return false;
        }

        let pager = match &mut self.ui.pager {
            Some(p) => p,
            None => return false,
        };

        match pager.handle_key(k) {
            PagerAction::Close => {
                self.ui.pager = None;
            }
            PagerAction::OpenSearch | PagerAction::OpenSearchBackward => {
                self.ui.search.query.clear();
                self.ui.search.current = 0;
                self.ui.search.update_matches(&self.chat.lines);
                self.ui.search.active = true;
            }
            PagerAction::SearchNext => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current =
                        (self.ui.search.current + 1) % self.ui.search.matches.len();
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(pager) = &mut self.ui.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::SearchPrev => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current = self
                        .ui
                        .search
                        .current
                        .checked_sub(1)
                        .unwrap_or(self.ui.search.matches.len() - 1);
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(pager) = &mut self.ui.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::Handled => {}
        }
        false
    }

    /// Handle a key event while the inspector overlay is open.
    ///
    /// The inspector pager uses the same search state as the main pager but
    /// scopes matches to the inspector's own lines.
    pub(crate) async fn handle_inspector_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crate::keys::map_search_key;
        use crate::pager::PagerAction;

        if self.ui.search.active {
            if let Some(action) = map_search_key(k) {
                return self.dispatch(action).await;
            }
            return false;
        }

        let inspector = match &mut self.ui.inspector {
            Some(i) => i,
            None => return false,
        };

        match inspector.pager.handle_key(k) {
            PagerAction::Close => {
                self.ui.inspector = None;
                self.ui.search.active = false;
            }
            PagerAction::OpenSearch => {
                self.ui.search.query.clear();
                self.ui.search.current = 0;
                let lines = inspector.pager.cloned_lines();
                self.ui.search.update_matches(&lines);
                // Jump to first match immediately.
                if let Some(line) = self.ui.search.current_line() {
                    if let Some(insp) = &mut self.ui.inspector {
                        insp.pager.scroll_to_line(line);
                    }
                }
                self.ui.search.active = true;
            }
            PagerAction::OpenSearchBackward => {
                self.ui.search.query.clear();
                self.ui.search.current = 0;
                let lines = inspector.pager.cloned_lines();
                self.ui.search.update_matches(&lines);
                // Start from the last match for backward search.
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current = self.ui.search.matches.len() - 1;
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_to_line(line);
                        }
                    }
                }
                self.ui.search.active = true;
            }
            PagerAction::SearchNext => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current =
                        (self.ui.search.current + 1) % self.ui.search.matches.len();
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(inspector) = &mut self.ui.inspector {
                            inspector.pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::SearchPrev => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current = self
                        .ui
                        .search
                        .current
                        .checked_sub(1)
                        .unwrap_or(self.ui.search.matches.len() - 1);
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(inspector) = &mut self.ui.inspector {
                            inspector.pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::Handled => {}
        }
        false
    }
}
