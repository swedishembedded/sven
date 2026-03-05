// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Input pane widget — Claude-style open layout with `>` prompt indicator,
//! top/bottom HR lines only (no left/right borders for clean copy-paste),
//! attachment bullet lines, character counter, and optional vertical scrollbar.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::app::input_state::InputAttachment;
use crate::input_wrap::wrap_content;

use super::theme::open_pane_block;

// ── InputEditMode ─────────────────────────────────────────────────────────────

/// What kind of content the input box is currently editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEditMode {
    Normal,
    Segment,
    Queue,
}

// ── PROMPT_WIDTH ──────────────────────────────────────────────────────────────

/// Columns reserved for the `> ` prompt gutter on the left.
pub const PROMPT_WIDTH: u16 = 2;

// ── InputPane widget ──────────────────────────────────────────────────────────

/// Claude-style multi-line input box.
///
/// Layout (top-to-bottom inside the pane area):
///   row 0      : ─────────────── title/hints (top HR, from Block)
///   rows 1..N-2: attachments (one row each), then text lines with `> ` prefix
///   row N-1    : ─────────────── hints + char count (bottom HR, from Block)
pub struct InputPane<'a> {
    pub content: &'a str,
    pub cursor_pos: usize,
    pub scroll_offset: usize,
    pub focused: bool,
    pub ascii: bool,
    pub edit_mode: InputEditMode,
    /// File/image attachments to display as bullet rows above the text.
    pub attachments: &'a [InputAttachment],
}

impl Widget for InputPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title: &str = match self.edit_mode {
            InputEditMode::Queue => "Edit queue",
            InputEditMode::Segment => "Edit",
            InputEditMode::Normal => "Input",
        };

        // Hints appear on the bottom border line (right-aligned).
        let hint: &str = match self.edit_mode {
            InputEditMode::Queue => "Enter:update  Esc:cancel",
            InputEditMode::Segment => "Enter:confirm  Esc:cancel",
            InputEditMode::Normal => "Enter:send  Alt+Enter:newline  ^↑↓:history  ^w k:chat",
        };

        // Character count shown next to the hint.
        let char_count = self.content.chars().count();
        let token_est = char_count / 4;
        let counter_str = if char_count > 0 {
            if self.ascii {
                format!("  {char_count}c ~{token_est}t  ")
            } else {
                format!("  {char_count}c ≈{token_est}t  ")
            }
        } else {
            String::new()
        };

        let block = open_pane_block(title, self.focused, self.ascii);
        let inner = block.inner(area);
        // Clear the inner area first so stale characters from a previous frame
        // (e.g. when content shrinks or scrolls away) are always erased.
        Clear.render(inner, buf);
        block.render(area, buf);

        // Render the hint + counter on the BOTTOM border row (right-aligned).
        // The bottom border is at `area.y + area.height - 1`.
        if area.height >= 2 {
            let bottom_y = area.y + area.height - 1;
            let hint_text = format!("{hint}{counter_str}");
            let hint_chars: String = hint_text
                .chars()
                .take(area.width.saturating_sub(2) as usize)
                .collect();
            Paragraph::new(Line::from(vec![Span::styled(
                hint_chars,
                Style::default().fg(Color::DarkGray),
            )]))
            .alignment(Alignment::Right)
            .render(Rect::new(area.x, bottom_y, area.width, 1), buf);
        }

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // ── Attachment bullets ────────────────────────────────────────────────
        let attach_rows = self.attachments.len().min(inner.height as usize / 2);
        let text_start_y = inner.y + attach_rows as u16;
        let text_height = inner.height.saturating_sub(attach_rows as u16);

        for (i, att) in self.attachments.iter().take(attach_rows).enumerate() {
            let y = inner.y + i as u16;
            let icon = att.icon(self.ascii);
            let name = att.display_name();
            let full = att.full_path();
            let avail = inner.width.saturating_sub(2) as usize;
            let display = if full.len() > avail && name.len() < avail {
                format!("{icon}{name}")
            } else {
                let s: String = format!("{icon}{full}").chars().take(avail).collect();
                s
            };
            buf.set_string(
                inner.x,
                y,
                &display,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC),
            );
        }

        if text_height == 0 {
            return;
        }

        // ── Text content with `>` prompt ──────────────────────────────────────
        // Reserve PROMPT_WIDTH columns for the `> ` indicator.
        let text_width_with_prompt = inner.width;
        let text_width = text_width_with_prompt.saturating_sub(PROMPT_WIDTH);
        if text_width == 0 {
            return;
        }

        let visible_height = text_height as usize;

        // Two-pass: first probe to decide if scrollbar is needed.
        let probe = wrap_content(self.content, text_width as usize, self.cursor_pos);
        let needs_scrollbar = probe.lines.len() > visible_height;
        let effective_text_width = if needs_scrollbar && text_width > 1 {
            text_width - 1
        } else {
            text_width
        };

        let wrap = if needs_scrollbar && text_width > 1 {
            wrap_content(self.content, effective_text_width as usize, self.cursor_pos)
        } else {
            probe
        };

        let total_lines = wrap.lines.len();
        let scroll = self
            .scroll_offset
            .min(total_lines.saturating_sub(visible_height));

        // Prompt style
        let prompt_focused = Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
        let prompt_unfocused = Style::default().fg(Color::DarkGray);
        let prompt_str = if self.ascii { "> " } else { "> " };
        let cont_str = "  "; // continuation lines: indent to align under text

        let prompt_x = inner.x;
        let text_x = inner.x + PROMPT_WIDTH;

        for (vis_row, wrapped_line) in wrap
            .lines
            .iter()
            .skip(scroll)
            .take(visible_height)
            .enumerate()
        {
            let y = text_start_y + vis_row as u16;
            // Render the `>` prefix for the first visible line; `  ` for rest.
            let (prefix, prefix_style) = if vis_row == 0 && scroll == 0 {
                (
                    prompt_str,
                    if self.focused {
                        prompt_focused
                    } else {
                        prompt_unfocused
                    },
                )
            } else {
                (cont_str, Style::default())
            };
            buf.set_string(prompt_x, y, prefix, prefix_style);

            // Render the text content.
            let text_line = Line::from(wrapped_line.clone());
            Paragraph::new(text_line).render(Rect::new(text_x, y, effective_text_width, 1), buf);
        }

        // ── Scrollbar ─────────────────────────────────────────────────────────
        if needs_scrollbar && text_width > 1 {
            let sb_x = text_x + effective_text_width;
            let sb_area = Rect::new(sb_x, text_start_y, 1, text_height);
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

// ── input_cursor_screen_pos ───────────────────────────────────────────────────

/// Compute the screen position of the text cursor within the input pane.
///
/// Accounts for the 2-column `> ` prompt and attachment rows.
pub fn input_cursor_screen_pos(
    area: Rect,
    content: &str,
    cursor_pos: usize,
    scroll_offset: usize,
    focused: bool,
    ascii: bool,
    edit_mode: InputEditMode,
    attachment_count: usize,
) -> Option<(u16, u16)> {
    if !focused {
        return None;
    }
    let title: &str = match edit_mode {
        InputEditMode::Queue => "Edit queue",
        InputEditMode::Segment => "Edit",
        InputEditMode::Normal => "Input",
    };
    let block = open_pane_block(title, focused, ascii);
    let inner = block.inner(area);

    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let attach_rows = attachment_count.min(inner.height as usize / 2) as u16;
    let text_height = inner.height.saturating_sub(attach_rows);
    let text_start_y = inner.y + attach_rows;

    let text_width = inner.width.saturating_sub(PROMPT_WIDTH);
    if text_width == 0 || text_height == 0 {
        return None;
    }

    let visible_height = text_height as usize;
    let probe = wrap_content(content, text_width as usize, cursor_pos);
    let needs_scrollbar = probe.lines.len() > visible_height;
    let effective_text_width = if needs_scrollbar && text_width > 1 {
        text_width - 1
    } else {
        text_width
    };
    let wrap = if needs_scrollbar && text_width > 1 {
        wrap_content(content, effective_text_width as usize, cursor_pos)
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
    let col = (wrap.cursor_col as u16).min(effective_text_width.saturating_sub(1));
    Some((inner.x + PROMPT_WIDTH + col, text_start_y + vis_row))
}
