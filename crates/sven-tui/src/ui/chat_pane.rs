// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane widget — borderless (top+bottom HR only) scrollable markdown display.
//!
//! The pane uses only TOP and BOTTOM borders so that terminal text selection
//! does not capture `│` border characters, enabling clean copy-paste.

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::markdown::StyledLines;
use crate::pager::{highlight_match_in_line, tint_match_line};

use super::theme::{open_pane_block, BG};
use super::width_utils::truncate_to_width_exact;

// ── ChatPane widget ───────────────────────────────────────────────────────────

/// Scrollable chat pane with open (top+bottom only) borders.
/// Segment actions (yank, edit, rerun, delete) are keyboard-first: use y/e/r/x
/// when the chat pane is focused; the focused segment is determined by scroll position.
pub struct ChatPane<'a> {
    pub lines: &'a StyledLines,
    pub scroll_offset: u16,
    pub focused: bool,
    pub ascii: bool,
    pub search_query: &'a str,
    pub search_matches: &'a [usize],
    pub search_current: usize,
    pub search_regex: Option<&'a regex::Regex>,
    pub editing_line_range: Option<(usize, usize)>,
    pub no_nvim: bool,
    /// Total number of conversation segments (for the title counter).
    pub segment_count: usize,
    /// True when the user has scrolled up and auto-scroll is paused.
    pub auto_scroll_paused: bool,
    /// Active mouse drag selection: `(start_abs_line, start_col, end_abs_line, end_col)`.
    /// Columns are relative to the inner area left edge.  `None` = no selection.
    pub selection: Option<(usize, u16, usize, u16)>,
    /// Line range (start, end) of the keyboard-highlighted segment when chat has focus.
    /// Drawn as a subtle full-line highlight; actions (e/y/r/x) apply to this segment.
    pub highlight_line_range: Option<(usize, usize)>,
}

impl Widget for ChatPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = if self.segment_count > 0 {
            format!("Chat  [{} msgs]", self.segment_count)
        } else {
            "Chat".to_string()
        };

        let block = open_pane_block(&title, self.focused, self.ascii);
        let inner = block.inner(area);
        // Clear inner area before rendering to prevent stale cells from
        // persisting when content shrinks or the scroll offset changes.
        Clear.render(inner, buf);
        block.render(area, buf);

        let match_set: HashSet<usize> = self.search_matches.iter().copied().collect();
        let current_match_line = self.search_matches.get(self.search_current).copied();

        // Reserve the last row of inner for the auto-scroll banner if needed.
        let banner_reserved = if self.auto_scroll_paused && inner.height > 2 {
            1u16
        } else {
            0
        };
        let content_height = inner.height.saturating_sub(banner_reserved);
        let total_lines = self.lines.len();
        let visible_height = content_height as usize;
        let show_scrollbar = self.no_nvim && inner.width > 1 && total_lines > visible_height;
        // When scrollbar is visible, keep content one column left so the scrollbar
        // column is never overwritten by paragraph/content (avoids stuck thumb/track).
        let content_width = if show_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        let visible: Vec<Line<'static>> = self
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll_offset as usize)
            .take(content_height as usize)
            .map(|(i, line)| {
                let is_current = !self.search_query.is_empty() && current_match_line == Some(i);
                let is_other =
                    !self.search_query.is_empty() && !is_current && match_set.contains(&i);
                if is_current {
                    highlight_match_in_line(line.clone(), self.search_query, self.search_regex)
                } else if is_other {
                    tint_match_line(line.clone())
                } else {
                    line.clone()
                }
            })
            .collect();

        // Content rect: only this region may be tinted (keeps highlights inside chat pane).
        let content_rect = Rect::new(inner.x, inner.y, content_width, content_height);

        Paragraph::new(visible)
            .style(Style::default().bg(BG))
            .render(content_rect, buf);

        // ── Segment highlight (j/k selection; clipped to content_rect) ───────
        if let Some((seg_start, seg_end)) = self.highlight_line_range {
            let highlight_style = Style::default().bg(Color::Rgb(35, 45, 55));
            for vis_row in 0..content_height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= seg_start && abs_line < seg_end {
                    let y = inner.y + vis_row as u16;
                    let row_rect = Rect::new(inner.x, y, content_width, 1);
                    let clipped = content_rect.intersection(row_rect);
                    if !clipped.is_empty() {
                        buf.set_style(clipped, highlight_style);
                    }
                }
            }
        }

        // ── Edit highlight (clipped to content_rect) ─────────────────────────
        if let Some((seg_start, seg_end)) = self.editing_line_range {
            let edit_style = Style::default().bg(Color::Rgb(0, 45, 65));
            for vis_row in 0..content_height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= seg_start && abs_line < seg_end {
                    let y = inner.y + vis_row as u16;
                    let row_rect = Rect::new(inner.x, y, content_width, 1);
                    let clipped = content_rect.intersection(row_rect);
                    if !clipped.is_empty() {
                        buf.set_style(clipped, edit_style);
                    }
                }
            }
        }

        // ── Mouse drag selection highlight (clipped to content_rect) ────────
        if let Some((s_line, s_col, e_line, e_col)) = self.selection {
            let sel_bg = Color::Rgb(50, 80, 135);
            let sel_fg = Color::Rgb(220, 220, 230);
            for vis_row in 0..content_height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= s_line && abs_line <= e_line {
                    let y = inner.y + vis_row as u16;
                    let from_x = if abs_line == s_line {
                        (inner.x + s_col).min(inner.x + content_width)
                    } else {
                        inner.x
                    };
                    let to_x = if abs_line == e_line {
                        (inner.x + e_col).min(inner.x + content_width)
                    } else {
                        inner.x + content_width
                    };
                    let w = to_x.saturating_sub(from_x);
                    if w == 0 {
                        continue;
                    }
                    let row_rect = Rect::new(from_x, y, w, 1);
                    let clipped = content_rect.intersection(row_rect);
                    if !clipped.is_empty() {
                        buf.set_style(clipped, Style::default().bg(sel_bg));
                        // Ensure text remains legible over the selection background.
                        for col in clipped.x..clipped.x + clipped.width {
                            if let Some(cell) = buf.cell_mut((col, y)) {
                                if cell.fg == Color::Reset {
                                    cell.set_fg(sel_fg);
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── Scrollbar (rightmost column) ──────────────────────────────────────
        if show_scrollbar {
            let sb_x = inner.x + inner.width - 1;
            let sb_area = Rect::new(sb_x, inner.y, 1, content_height);
            // Clear the scrollbar column before drawing so previous thumb/track
            // positions don't persist (avoids "stuck" scrollbar bits).
            Clear.render(sb_area, buf);
            let scrollable_range = total_lines.saturating_sub(visible_height) + 1;
            let mut sb_state = ScrollbarState::new(scrollable_range)
                .position(self.scroll_offset as usize)
                .viewport_content_length(visible_height);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_symbol("|")
                .track_symbol(Some("░"))
                .render(sb_area, buf, &mut sb_state);
        }

        // ── Auto-scroll paused banner ─────────────────────────────────────────
        if self.auto_scroll_paused && banner_reserved > 0 {
            let banner_y = inner.y + content_height;
            let msg = if self.ascii {
                "v  New content below -- press G to scroll to bottom  v"
            } else {
                "↓  New content below  ·  press G to jump to bottom  ↓"
            };
            let msg_chars = truncate_to_width_exact(msg, inner.width as usize);
            // Fill the banner row with the highlighted background.
            for col in inner.x..inner.x + inner.width {
                buf[(col, banner_y)].set_bg(Color::Rgb(60, 45, 0));
            }
            Paragraph::new(Line::from(vec![Span::styled(
                msg_chars,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]))
            .alignment(Alignment::Center)
            .render(Rect::new(inner.x, banner_y, inner.width, 1), buf);
        }
    }
}

// ── nvim_cursor_screen_pos ────────────────────────────────────────────────────

/// Compute the screen position of the Neovim cursor inside the chat pane.
pub fn nvim_cursor_screen_pos(
    inner: Rect,
    cursor: (u16, u16),
    scroll_offset: u16,
    focused: bool,
) -> Option<(u16, u16)> {
    if !focused {
        return None;
    }
    let (cursor_row, cursor_col) = cursor;
    let visible_row = cursor_row.checked_sub(scroll_offset)?;
    if (visible_row as usize) >= inner.height as usize {
        return None;
    }
    Some((
        inner.x + cursor_col.min(inner.width.saturating_sub(1)),
        inner.y + visible_row,
    ))
}
