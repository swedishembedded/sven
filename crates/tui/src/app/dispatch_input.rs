// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Raw input-buffer editing keybindings: cursor motion, character/word/line
//! deletion, history navigation, and input-pane resizing.  Pure text-buffer
//! mutation with no agent or session I/O.
//!
//! Also home to [`App::apply_input_to_edit`] (the twin of this dispatcher
//! that redirects the same key set into the inline chat-segment/queue-item
//! edit buffer instead of the input box - see `dispatch.rs`'s `edit.active()`
//! check) and the character/word-boundary helpers both use.

use crate::{app::App, keys::Action};

impl App {
    /// Dispatch the raw input-buffer editing subset of [`Action`]. Returns
    /// `true` when `dispatch()` should stop processing further (currently
    /// only `InputHistoryUp`/`InputHistoryDown`, which mirror `dispatch()`'s
    /// default `false` but make the "no further action" intent explicit).
    pub(crate) async fn dispatch_input(&mut self, action: Action) -> bool {
        match action {
            Action::InputChar(c) => {
                self.input.buffer.insert(self.input.cursor, c);
                self.input.cursor += c.len_utf8();
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.ui.completion = None;
                }
            }
            Action::InputNewline => {
                self.input.buffer.insert(self.input.cursor, '\n');
                self.input.cursor += 1;
                self.ui.completion = None;
            }
            Action::InputBackspace => {
                if self.input.cursor > 0 {
                    let prev = prev_char_boundary(&self.input.buffer, self.input.cursor);
                    self.input.buffer.remove(prev);
                    self.input.cursor = prev;
                }
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.ui.completion = None;
                }
            }
            Action::InputDelete => {
                if self.input.cursor < self.input.buffer.len() {
                    self.input.buffer.remove(self.input.cursor);
                }
            }
            Action::InputMoveCursorLeft => {
                self.input.cursor = prev_char_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveCursorRight => {
                if self.input.cursor < self.input.buffer.len() {
                    let ch = self.input.buffer[self.input.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    self.input.cursor += ch;
                }
            }
            Action::InputMoveWordLeft => {
                self.input.cursor = prev_word_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveWordRight => {
                self.input.cursor = next_word_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveLineStart => self.input.cursor = 0,
            Action::InputMoveLineEnd => self.input.cursor = self.input.buffer.len(),
            Action::InputMoveLineUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
                    if ws.cursor_row > 0 {
                        // Move cursor up within the multi-line text.
                        self.input.cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input.buffer,
                            w,
                            ws.cursor_row - 1,
                            ws.cursor_col,
                        );
                    } else {
                        // Already on the first visual row - cycle to the older history entry.
                        if let Some(entry) = self.input.history_up() {
                            let text = entry.to_string();
                            self.input.cursor = text.len();
                            self.input.buffer = text;
                            self.input.scroll_offset = 0;
                        }
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        // Move cursor down within the multi-line text.
                        self.input.cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input.buffer,
                            w,
                            ws.cursor_row + 1,
                            ws.cursor_col,
                        );
                    } else {
                        // Already on the last visual row - cycle to the newer history entry.
                        if let Some(entry) = self.input.history_down() {
                            let text = entry.to_string();
                            self.input.cursor = text.len();
                            self.input.buffer = text;
                            self.input.scroll_offset = 0;
                        }
                    }
                }
            }
            Action::InputPageUp => {
                let h = self.layout.input_inner_height as usize;
                if self.edit.active() {
                    self.edit.scroll_offset = self.edit.scroll_offset.saturating_sub(h);
                } else {
                    self.input.scroll_offset = self.input.scroll_offset.saturating_sub(h);
                }
            }
            Action::InputPageDown => {
                let w = self.layout.input_inner_width as usize;
                let h = self.layout.input_inner_height as usize;
                if w > 0 && h > 0 {
                    let in_edit = self.edit.active();
                    let content = if in_edit {
                        &self.edit.buffer
                    } else {
                        &self.input.buffer
                    };
                    let ws = crate::input_wrap::wrap_content(content, w, 0);
                    let max = ws.lines.len().saturating_sub(h);
                    if in_edit {
                        self.edit.scroll_offset = (self.edit.scroll_offset + h).min(max);
                    } else {
                        self.input.scroll_offset = (self.input.scroll_offset + h).min(max);
                    }
                }
            }
            Action::InputDeleteToEnd => self.input.buffer.truncate(self.input.cursor),
            Action::InputDeleteToStart => {
                self.input.buffer = self.input.buffer[self.input.cursor..].to_string();
                self.input.cursor = 0;
            }

            Action::InputHistoryUp => {
                if let Some(entry) = self.input.history_up() {
                    let text = entry.to_string();
                    self.input.cursor = text.len();
                    self.input.buffer = text;
                    self.input.scroll_offset = 0;
                }
                return false;
            }

            Action::InputHistoryDown => {
                if let Some(entry) = self.input.history_down() {
                    let text = entry.to_string();
                    self.input.cursor = text.len();
                    self.input.buffer = text;
                    self.input.scroll_offset = 0;
                }
                return false;
            }

            Action::ResizeInputGrow => {
                self.prefs.input_height = (self.prefs.input_height + 1).min(20);
            }

            Action::ResizeInputShrink => {
                self.prefs.input_height = (self.prefs.input_height - 1).max(3);
            }

            Action::InputScrollUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let in_edit = self.edit.active();
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
            }

            Action::InputScrollDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let in_edit = self.edit.active();
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
            }

            _ => {}
        }
        false
    }

    // ── Edit-buffer redirect ────────────────────────────────────────────────────

    /// Route an input-editing `Action` into the inline edit buffer
    /// (`self.edit`) instead of the normal input box, when a chat-segment or
    /// queue-item edit is in progress. Returns `None` for actions that don't
    /// apply to a text buffer (nav, submit, etc.), letting `dispatch()` fall
    /// through to its normal handling.
    pub(crate) fn apply_input_to_edit(&self, action: &Action) -> Option<(String, usize)> {
        let (buf, cur) = (&self.edit.buffer, self.edit.cursor);
        let mut buf = buf.clone();
        let mut cur = cur;
        match action {
            Action::InputChar(c) => {
                buf.insert(cur, *c);
                cur += c.len_utf8();
            }
            Action::InputNewline => {
                buf.insert(cur, '\n');
                cur += 1;
            }
            Action::InputBackspace => {
                if cur > 0 {
                    let prev = prev_char_boundary(&buf, cur);
                    buf.remove(prev);
                    cur = prev;
                }
            }
            Action::InputDelete => {
                if cur < buf.len() {
                    buf.remove(cur);
                }
            }
            Action::InputMoveCursorLeft => cur = prev_char_boundary(&buf, cur),
            Action::InputMoveCursorRight => {
                if cur < buf.len() {
                    let ch = buf[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    cur += ch;
                }
            }
            Action::InputMoveWordLeft => cur = prev_word_boundary(&buf, cur),
            Action::InputMoveWordRight => cur = next_word_boundary(&buf, cur),
            Action::InputMoveLineStart => cur = 0,
            Action::InputMoveLineEnd => cur = buf.len(),
            Action::InputMoveLineUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row > 0 {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf,
                            w,
                            ws.cursor_row - 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf,
                            w,
                            ws.cursor_row + 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputDeleteToEnd => buf.truncate(cur),
            Action::InputDeleteToStart => {
                buf = buf[cur..].to_string();
                cur = 0;
            }
            _ => return None,
        }
        Some((buf, cur))
    }
}

// ── Character and word boundary helpers ──────────────────────────────────────

pub(crate) fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

pub(crate) fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[..pos];
    let trimmed = bytes
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[..trimmed]
        .iter()
        .rposition(|&b| b == b' ')
        .map(|i| i + 1)
        .unwrap_or(0)
}

pub(crate) fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[pos..];
    let start = bytes.iter().position(|&b| b != b' ').unwrap_or(0);
    let end = bytes[start..]
        .iter()
        .position(|&b| b == b' ')
        .unwrap_or(bytes.len() - start);
    pos + start + end
}
