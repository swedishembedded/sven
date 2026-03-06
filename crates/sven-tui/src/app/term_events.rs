// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Terminal event handler: keyboard, mouse, and resize dispatch.

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::layout::Rect;
use sven_model::{MessageContent, Role};

use crate::{
    app::input_state::{is_image_path, InputAttachment},
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
                if self.ui.show_help {
                    self.ui.show_help = false;
                    return false;
                }
                if self.ui.question_modal.is_some() {
                    return self.handle_modal_key(k);
                }
                if self.ui.confirm_modal.is_some() {
                    return self.handle_confirm_modal_key(k).await;
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
                if let Some(action) = map_key(
                    k,
                    in_search,
                    in_input,
                    self.ui.pending_nav,
                    in_edit_mode,
                    in_queue,
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
                if self.ui.pager.is_none() {
                    let over_input = mouse.row >= self.layout.input_pane.y
                        && mouse.row < self.layout.input_pane.y + self.layout.input_pane.height;
                    let over_queue = self.layout.queue_pane.height > 0
                        && mouse.row >= self.layout.queue_pane.y
                        && mouse.row < self.layout.queue_pane.y + self.layout.queue_pane.height;
                    let in_edit =
                        self.edit.message_index.is_some() || self.edit.queue_index.is_some();

                    // ── Chat content geometry (used by selection handlers) ─────────
                    let chat_content_x = self.layout.chat_pane.x;
                    let chat_content_top = self.layout.chat_pane.y + 1;
                    let chat_content_h = self.layout.chat_pane.height.saturating_sub(2);
                    let chat_content_bottom = chat_content_top + chat_content_h;

                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if over_input {
                                let w = self.layout.input_inner_width as usize;
                                if w > 0 {
                                    let (buf, cursor) = if in_edit {
                                        (self.edit.buffer.clone(), self.edit.cursor)
                                    } else {
                                        (self.input.buffer.clone(), self.input.cursor)
                                    };
                                    let wrap = crate::input_wrap::wrap_content(&buf, w, cursor);
                                    let new_row = wrap.cursor_row.saturating_sub(1);
                                    let new_cursor = crate::input_wrap::byte_offset_at_row_col(
                                        &buf,
                                        w,
                                        new_row,
                                        wrap.cursor_col,
                                    );
                                    if in_edit {
                                        self.edit.cursor = new_cursor;
                                    } else {
                                        self.input.cursor = new_cursor;
                                    }
                                }
                            } else if self.nvim.bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim.bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-y>").await;
                                }
                            } else {
                                self.scroll_up(1);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if over_input {
                                let w = self.layout.input_inner_width as usize;
                                if w > 0 {
                                    let (buf, cursor) = if in_edit {
                                        (self.edit.buffer.clone(), self.edit.cursor)
                                    } else {
                                        (self.input.buffer.clone(), self.input.cursor)
                                    };
                                    let wrap = crate::input_wrap::wrap_content(&buf, w, cursor);
                                    let max_row = wrap.lines.len().saturating_sub(1);
                                    let new_row = (wrap.cursor_row + 1).min(max_row);
                                    let new_cursor = crate::input_wrap::byte_offset_at_row_col(
                                        &buf,
                                        w,
                                        new_row,
                                        wrap.cursor_col,
                                    );
                                    if in_edit {
                                        self.edit.cursor = new_cursor;
                                    } else {
                                        self.input.cursor = new_cursor;
                                    }
                                }
                            } else if self.nvim.bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim.bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-e>").await;
                                }
                            } else {
                                self.scroll_down(1);
                            }
                        }
                        MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            if self.nvim.disabled =>
                        {
                            // ── Selection anchor ──────────────────────────────────
                            // Record the anchor for a potential drag selection.
                            // Any previous completed selection is cleared.
                            let over_chat_for_sel = mouse.row >= chat_content_top
                                && mouse.row < chat_content_bottom
                                && !over_queue
                                && !over_input;
                            if over_chat_for_sel {
                                let abs_line = (mouse.row - chat_content_top) as usize
                                    + self.chat.scroll_offset as usize;
                                let inner_col = mouse
                                    .column
                                    .saturating_sub(chat_content_x)
                                    .min(self.layout.chat_pane.width.saturating_sub(1));
                                self.chat.selection_anchor = Some((abs_line, inner_col));
                                self.chat.selection_end = None;
                                self.chat.is_selecting = false;
                            } else {
                                self.chat.selection_anchor = None;
                                self.chat.selection_end = None;
                                self.chat.is_selecting = false;
                            }

                            // ── Click on queue panel ──────────────────────────────
                            if over_queue && !self.queue.messages.is_empty() {
                                let inner_y = self.layout.queue_pane.y + 1; // skip border
                                if mouse.row >= inner_y {
                                    let item_idx = (mouse.row - inner_y) as usize;
                                    if item_idx < self.queue.messages.len() {
                                        self.queue.selected = Some(item_idx);
                                        self.ui.focus = FocusPane::Queue;
                                        if let Some(qm) = self.queue.messages.get(item_idx) {
                                            let text = qm.content.clone();
                                            self.edit.queue_index = Some(item_idx);
                                            self.edit.cursor = text.len();
                                            self.edit.original_text = Some(text.clone());
                                            self.edit.buffer = text;
                                            self.ui.focus = FocusPane::Input;
                                        }
                                    }
                                }
                            }

                            // ── Click on chat pane ────────────────────────────────
                            // TOP+BOTTOM-only borders: inner.x == pane.x, inner.width == pane.width.
                            let chat_inner_x = self.layout.chat_pane.x;
                            let chat_inner_w = self.layout.chat_pane.width;
                            let chat_inner_h = self.layout.chat_pane.height.saturating_sub(2);
                            let content_start_row = self.layout.chat_pane.y + 1;

                            // ── Scrollbar click ───────────────────────────────────
                            let scrollbar_col = chat_inner_x + chat_inner_w.saturating_sub(1);
                            let total_chat_lines = self.chat.lines.len() as u16;
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
                                self.chat.scroll_offset =
                                    new_offset.min(total_chat_lines.saturating_sub(chat_inner_h));
                                self.chat.auto_scroll = false;
                                self.recompute_focused_segment();
                            }

                            let clicked_scrollbar =
                                mouse.column == scrollbar_col && total_chat_lines > chat_inner_h;
                            if mouse.row >= content_start_row
                                && !over_queue
                                && !over_input
                                && !clicked_scrollbar
                            {
                                let click_line = (mouse.row - content_start_row) as usize
                                    + self.chat.scroll_offset as usize;
                                if let Some(seg_idx) =
                                    segment_at_line(&self.chat.segment_line_ranges, click_line)
                                {
                                    let _is_editable =
                                        segment_editable_text(&self.chat.segments, seg_idx)
                                            .is_some();

                                    // Detect clicks on right-aligned action icons.
                                    // Layout (9 cols from inner right edge, scrollbar at w-1):
                                    //   y copy   — col w-9, w-8  (zone [w-9, w-7])
                                    //   ↻ rerun  — col w-7, w-6  (zone [w-7, w-5])
                                    //   ✎ edit   — col w-5, w-4  (zone [w-5, w-3])
                                    //   ✕ delete — col w-3, w-2  (zone [w-3, w-1])
                                    let is_header_line =
                                        self.chat.remove_labels.contains(&click_line);

                                    let label_area_start =
                                        chat_inner_x + chat_inner_w.saturating_sub(9);
                                    let rerun_zone_start =
                                        chat_inner_x + chat_inner_w.saturating_sub(7);
                                    let edit_zone_start =
                                        chat_inner_x + chat_inner_w.saturating_sub(5);
                                    let delete_zone_start =
                                        chat_inner_x + chat_inner_w.saturating_sub(3);

                                    let clicked_copy = is_header_line
                                        && self.chat.copy_labels.contains(&click_line)
                                        && mouse.column >= label_area_start
                                        && mouse.column < rerun_zone_start;
                                    let clicked_delete = is_header_line
                                        && mouse.column >= delete_zone_start
                                        && mouse.column
                                            < chat_inner_x + chat_inner_w.saturating_sub(1);
                                    let clicked_edit = is_header_line
                                        && self.chat.edit_labels.contains(&click_line)
                                        && mouse.column >= edit_zone_start
                                        && mouse.column < delete_zone_start;
                                    let clicked_rerun = is_header_line
                                        && self.chat.rerun_labels.contains(&click_line)
                                        && mouse.column >= rerun_zone_start
                                        && mouse.column < edit_zone_start;
                                    let outside_labels = mouse.column < label_area_start;

                                    if clicked_delete {
                                        use crate::overlay::confirm::{
                                            ConfirmModal, ConfirmedAction,
                                        };
                                        let preview =
                                            segment_short_preview(self.chat.segments.get(seg_idx));
                                        self.ui.confirm_modal = Some(ConfirmModal::new(
                                            "Delete message",
                                            format!(
                                                "Remove this message from the conversation?\n{preview}"
                                            ),
                                            ConfirmedAction::RemoveSegment(seg_idx),
                                        ));
                                    } else if clicked_edit {
                                        if let Some(text) =
                                            segment_editable_text(&self.chat.segments, seg_idx)
                                        {
                                            self.edit.message_index = Some(seg_idx);
                                            self.edit.cursor = text.len();
                                            self.edit.original_text = Some(text.clone());
                                            self.edit.buffer = text;
                                            self.ui.focus = FocusPane::Input;
                                            self.update_editing_segment_live();
                                            self.rerender_chat().await;
                                        }
                                    } else if clicked_copy {
                                        self.ui.confirm_modal = None;
                                        let saved = self.chat.focused_segment;
                                        self.chat.focused_segment = Some(seg_idx);
                                        self.dispatch(Action::CopySegment).await;
                                        if self.chat.focused_segment.is_some() {
                                            self.chat.focused_segment = saved;
                                        }
                                    } else if clicked_rerun {
                                        self.ui.confirm_modal = None;
                                        let saved = self.chat.focused_segment;
                                        self.chat.focused_segment = Some(seg_idx);
                                        self.dispatch(Action::RerunFromSegment).await;
                                        if self.chat.focused_segment.is_some() {
                                            self.chat.focused_segment = saved;
                                        }
                                    } else {
                                        if outside_labels {
                                            self.ui.confirm_modal = None;
                                        }
                                        // All other clicks: cycle expand level.
                                        let is_collapsible = match self.chat.segments.get(seg_idx) {
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
                                            // Cycle: 0 → 1 → 2 → 0
                                            if let Some(seg) = self.chat.segments.get(seg_idx) {
                                                let cur =
                                                    self.chat.effective_expand_level(seg_idx, seg);
                                                let next = (cur + 1) % 3;
                                                self.chat.expand_level.insert(seg_idx, next);
                                            }
                                            self.build_display_from_segments();
                                            self.ui.search.update_matches(&self.chat.lines);
                                            let max_offset = (self.chat.lines.len() as u16)
                                                .saturating_sub(self.layout.chat_height);
                                            self.chat.scroll_offset =
                                                self.chat.scroll_offset.min(max_offset);
                                            if let Some(&(seg_start, _)) =
                                                self.chat.segment_line_ranges.get(seg_idx)
                                            {
                                                if (seg_start as u16) < self.chat.scroll_offset {
                                                    self.chat.scroll_offset = seg_start as u16;
                                                }
                                            }
                                            self.recompute_focused_segment();
                                        }
                                    }
                                }
                            }
                        }
                        // ── Drag: extend drag selection ───────────────────────────
                        MouseEventKind::Drag(crossterm::event::MouseButton::Left)
                            if self.nvim.disabled =>
                        {
                            if self.chat.selection_anchor.is_some() && !over_input && !over_queue {
                                // Clamp the drag row to the visible chat area.
                                let clamped_row = mouse
                                    .row
                                    .clamp(chat_content_top, chat_content_bottom.saturating_sub(1));
                                let abs_line = (clamped_row - chat_content_top) as usize
                                    + self.chat.scroll_offset as usize;
                                let abs_line =
                                    abs_line.min(self.chat.lines.len().saturating_sub(1));
                                let inner_col = mouse
                                    .column
                                    .saturating_sub(chat_content_x)
                                    .min(self.layout.chat_pane.width.saturating_sub(1));
                                self.chat.selection_end = Some((abs_line, inner_col));
                                self.chat.is_selecting = true;

                                // Auto-scroll when dragging near the top / bottom edge.
                                const SCROLL_ZONE: u16 = 2;
                                if mouse.row < chat_content_top + SCROLL_ZONE {
                                    self.scroll_up(1);
                                } else if mouse.row
                                    >= chat_content_bottom.saturating_sub(SCROLL_ZONE)
                                {
                                    self.scroll_down(1);
                                }
                            }
                        }

                        // ── Up: finalise selection and copy ───────────────────────
                        MouseEventKind::Up(crossterm::event::MouseButton::Left)
                            if self.nvim.disabled =>
                        {
                            if self.chat.is_selecting {
                                self.copy_selection_to_clipboard();
                                // Keep anchor + end so the selection stays highlighted;
                                // it will be cleared on the next mouse-down.
                            }
                        }

                        _ => {}
                    }
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
            PagerAction::OpenSearch => {
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
}
