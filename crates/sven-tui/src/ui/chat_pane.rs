// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane widget — scrollable markdown display with search highlighting,
//! segment action labels, and optional Neovim cursor overlay.

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::markdown::StyledLines;
use crate::pager::{highlight_match_in_line, tint_match_line};

use super::theme::pane_block;

// ── ChatLabels ────────────────────────────────────────────────────────────────

/// Per-line action label descriptor for the chat pane.
///
/// The three icons are rendered right-aligned in the last 7 columns of the
/// inner chat area (1 scrollbar col + 3 × 2-col icon slots):
///
/// ```text
///   ↻ rerun  — col w-7, absorber w-6   click zone [w-7, w-5)
///   ✎ edit   — col w-5, absorber w-4   click zone [w-5, w-3)
///   ✕ delete — col w-3, absorber w-2   click zone [w-3, w-1)  scrollbar at w-1
/// ```
pub struct ChatLabels {
    pub edit_label_lines: HashSet<usize>,
    pub remove_label_lines: HashSet<usize>,
    pub rerun_label_lines: HashSet<usize>,
    /// Absolute line index of a segment awaiting delete confirmation (shown brightly).
    pub pending_delete_line: Option<usize>,
}

// ── ChatPane widget ───────────────────────────────────────────────────────────

/// Scrollable chat pane.
///
/// Rendering is side-effect-free — Neovim cursor placement is handled
/// externally by `App::view()` after this widget renders.
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
    pub labels: &'a ChatLabels,
    /// True when running without embedded Neovim — enables the scrollbar and
    /// action-label overlays.
    pub no_nvim: bool,
}

impl Widget for ChatPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = pane_block("Chat", self.focused, self.ascii);
        let inner = block.inner(area);
        block.render(area, buf);

        let match_set: HashSet<usize> = self.search_matches.iter().copied().collect();
        let current_match_line = self.search_matches.get(self.search_current).copied();

        let visible: Vec<Line<'static>> = self
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll_offset as usize)
            .take(inner.height as usize)
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

        // Background fill ensures stale cells from previous scroll positions
        // are overwritten on every frame.
        Paragraph::new(visible)
            .style(Style::default().bg(Color::Black))
            .render(inner, buf);

        // ── Full-row highlight for the segment being edited ───────────────────
        if let Some((seg_start, seg_end)) = self.editing_line_range {
            for vis_row in 0..inner.height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= seg_start && abs_line < seg_end {
                    let y = inner.y + vis_row as u16;
                    for x in inner.x..inner.x + inner.width {
                        buf[(x, y)].set_bg(Color::Rgb(0, 45, 65));
                    }
                }
            }
        }

        // ── Action label overlays (no-nvim mode only) ─────────────────────────
        // Columns (right-edge relative, scrollbar occupies the last col):
        //   ↻ rerun  col w-7, absorber w-6
        //   ✎ edit   col w-5, absorber w-4
        //   ✕ delete col w-3, absorber w-2
        //
        // Each icon + absorber cell: the absorber's symbol is set to ""
        // (Print("") is a no-op) so 2-wide glyphs in Nerd-Font terminals
        // never bleed into the adjacent column.
        if self.no_nvim && inner.width > 7 {
            let (icon_rerun, icon_edit, icon_delete) = if self.ascii {
                ("r", "e", "x")
            } else {
                ("↻", "✎", "✕")
            };

            let unavailable = Style::default().fg(Color::Rgb(50, 50, 50));
            let edit_active = Style::default().fg(Color::White);
            let rerun_active = Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM);
            let delete_style = Style::default().fg(Color::Red).add_modifier(Modifier::DIM);
            let confirm_style = Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD);

            let x_rerun = inner.x + inner.width.saturating_sub(7);
            let x_edit = inner.x + inner.width.saturating_sub(5);
            let x_delete = inner.x + inner.width.saturating_sub(3);

            for &abs_line in &self.labels.remove_label_lines {
                if abs_line < self.scroll_offset as usize {
                    continue;
                }
                let vis_row = abs_line - self.scroll_offset as usize;
                if vis_row >= inner.height as usize {
                    continue;
                }
                let y = inner.y + vis_row as u16;

                if self.labels.pending_delete_line == Some(abs_line) {
                    buf.set_string(x_rerun, y, "del?", confirm_style);
                } else {
                    let rs = if self.labels.rerun_label_lines.contains(&abs_line) {
                        rerun_active
                    } else {
                        unavailable
                    };
                    let es = if self.labels.edit_label_lines.contains(&abs_line) {
                        edit_active
                    } else {
                        unavailable
                    };
                    buf.set_string(x_rerun, y, icon_rerun, rs);
                    buf[(x_rerun + 1, y)].set_symbol("");
                    buf.set_string(x_edit, y, icon_edit, es);
                    buf[(x_edit + 1, y)].set_symbol("");
                    buf.set_string(x_delete, y, icon_delete, delete_style);
                    buf[(x_delete + 1, y)].set_symbol("");
                }
            }
        }

        // ── Scrollbar (no-nvim mode, rightmost column) ────────────────────────
        if self.no_nvim && inner.width > 1 {
            let total_lines = self.lines.len();
            let visible_height = inner.height as usize;
            if total_lines > visible_height {
                let sb_x = inner.x + inner.width - 1;
                let sb_area = Rect::new(sb_x, inner.y, 1, inner.height);
                let scrollable_range = total_lines.saturating_sub(visible_height) + 1;
                let mut sb_state = ScrollbarState::new(scrollable_range)
                    .position(self.scroll_offset as usize)
                    .viewport_content_length(visible_height);
                // Use narrow symbols only: '|' (ASCII, 1-wide) and '░' (U+2591,
                // width_cjk=1) avoid the 2-column Ambiguous-width issue that
                // makes the default '║'/'█' symbols garble the border column.
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_symbol("|")
                    .track_symbol(Some("░"))
                    .render(sb_area, buf, &mut sb_state);
            }
        }
    }
}

/// Compute the screen position of the Neovim cursor inside the chat pane.
///
/// Returns `(col, row)` in terminal coordinates, or `None` when the cursor
/// is outside the visible portion of the pane.
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
