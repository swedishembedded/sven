// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Input pane widget — multi-line text input with wrapping, scrollbar, and
//! cursor tracking.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    text::Line,
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::input_wrap::wrap_content;

use super::theme::pane_block;

// ── InputEditMode ─────────────────────────────────────────────────────────────

/// What kind of content the input box is currently editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEditMode {
    /// Normal message composition.
    Normal,
    /// Editing an existing chat-history segment.
    Segment,
    /// Editing a pending queue item.
    Queue,
}

// ── InputPane widget ──────────────────────────────────────────────────────────

/// Multi-line input box with wrapping and optional vertical scrollbar.
///
/// Cursor placement is **not** done inside this widget — the caller must
/// call `input_cursor_screen_pos()` and `frame.set_cursor_position()` after
/// rendering.
pub struct InputPane<'a> {
    pub content: &'a str,
    /// Byte index of the cursor inside `content`.
    pub cursor_pos: usize,
    pub scroll_offset: usize,
    pub focused: bool,
    pub ascii: bool,
    pub edit_mode: InputEditMode,
}

impl Widget for InputPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title: &str = match self.edit_mode {
            InputEditMode::Queue => "Edit queue  [Enter:update  Esc:cancel]",
            InputEditMode::Segment => "Edit  [Enter:confirm  Esc:cancel]",
            InputEditMode::Normal => "Input  [Enter:send  Shift+Enter:newline  ^w k:↑chat]",
        };

        let block = pane_block(title, self.focused, self.ascii);
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let visible_height = inner.height as usize;

        // First pass: probe without scrollbar to decide if one is needed.
        let probe = wrap_content(self.content, inner.width as usize, self.cursor_pos);
        let needs_scrollbar = probe.lines.len() > visible_height;

        let text_width = if needs_scrollbar && inner.width > 1 {
            inner.width - 1
        } else {
            inner.width
        };

        let wrap = if needs_scrollbar && inner.width > 1 {
            wrap_content(self.content, text_width as usize, self.cursor_pos)
        } else {
            probe
        };

        let total_lines = wrap.lines.len();
        let scroll = self
            .scroll_offset
            .min(total_lines.saturating_sub(visible_height));

        let text_area = Rect::new(inner.x, inner.y, text_width, inner.height);

        let visible: Vec<Line<'static>> = wrap
            .lines
            .iter()
            .skip(scroll)
            .take(visible_height)
            .map(|l| Line::from(l.clone()))
            .collect();
        Paragraph::new(visible).render(text_area, buf);

        if needs_scrollbar && inner.width > 1 {
            let sb_area = Rect::new(inner.x + text_width, inner.y, 1, inner.height);
            let scrollable_range = total_lines.saturating_sub(visible_height) + 1;
            let mut sb_state = ScrollbarState::new(scrollable_range)
                .position(scroll)
                .viewport_content_length(visible_height);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_symbol("|")
                .track_symbol(Some("░"))
                .render(sb_area, buf, &mut sb_state);
        }
    }
}

/// Compute the screen position of the text cursor within the input pane.
///
/// Returns `(col, row)` in terminal coordinates, or `None` when the cursor
/// row is outside the visible viewport or the pane is not focused.
pub fn input_cursor_screen_pos(
    area: Rect,
    content: &str,
    cursor_pos: usize,
    scroll_offset: usize,
    focused: bool,
    ascii: bool,
    edit_mode: InputEditMode,
) -> Option<(u16, u16)> {
    if !focused {
        return None;
    }
    let title: &str = match edit_mode {
        InputEditMode::Queue => "Edit queue  [Enter:update  Esc:cancel]",
        InputEditMode::Segment => "Edit  [Enter:confirm  Esc:cancel]",
        InputEditMode::Normal => "Input  [Enter:send  Shift+Enter:newline  ^w k:↑chat]",
    };
    let block = pane_block(title, focused, ascii);
    let inner = block.inner(area);

    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let visible_height = inner.height as usize;
    let probe = wrap_content(content, inner.width as usize, cursor_pos);
    let needs_scrollbar = probe.lines.len() > visible_height;
    let text_width = if needs_scrollbar && inner.width > 1 {
        inner.width - 1
    } else {
        inner.width
    };
    let wrap = if needs_scrollbar && inner.width > 1 {
        wrap_content(content, text_width as usize, cursor_pos)
    } else {
        probe
    };

    let total_lines = wrap.lines.len();
    let scroll = scroll_offset.min(total_lines.saturating_sub(visible_height));

    let cursor_row = wrap.cursor_row;
    if cursor_row < scroll || cursor_row >= scroll + visible_height {
        return None;
    }
    let vis_row = (cursor_row - scroll) as u16;
    let col = (wrap.cursor_col as u16).min(text_width.saturating_sub(1));
    Some((inner.x + col, inner.y + vis_row))
}
