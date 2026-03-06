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

// ── ChatLabels ────────────────────────────────────────────────────────────────

/// Per-line action label descriptor for the chat pane.
pub struct ChatLabels {
    pub edit_label_lines: HashSet<usize>,
    pub remove_label_lines: HashSet<usize>,
    pub rerun_label_lines: HashSet<usize>,
    pub copy_label_lines: HashSet<usize>,
    pub pending_delete_line: Option<usize>,
}

// ── ChatPane widget ───────────────────────────────────────────────────────────

/// Scrollable chat pane with open (top+bottom only) borders.
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
    pub no_nvim: bool,
    /// Total number of conversation segments (for the title counter).
    pub segment_count: usize,
    /// True when the user has scrolled up and auto-scroll is paused.
    pub auto_scroll_paused: bool,
    /// Active mouse drag selection: `(start_abs_line, start_col, end_abs_line, end_col)`.
    /// Columns are relative to the inner area left edge.  `None` = no selection.
    pub selection: Option<(usize, u16, usize, u16)>,
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

        Paragraph::new(visible)
            .style(Style::default().bg(BG))
            .render(
                Rect::new(inner.x, inner.y, inner.width, content_height),
                buf,
            );

        // ── Edit highlight ────────────────────────────────────────────────────
        if let Some((seg_start, seg_end)) = self.editing_line_range {
            for vis_row in 0..content_height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= seg_start && abs_line < seg_end {
                    let y = inner.y + vis_row as u16;
                    for x in inner.x..inner.x + inner.width {
                        buf[(x, y)].set_bg(Color::Rgb(0, 45, 65));
                    }
                }
            }
        }

        // ── Mouse drag selection highlight ────────────────────────────────────
        if let Some((s_line, s_col, e_line, e_col)) = self.selection {
            for vis_row in 0..content_height as usize {
                let abs_line = vis_row + self.scroll_offset as usize;
                if abs_line >= s_line && abs_line <= e_line {
                    let y = inner.y + vis_row as u16;
                    let from_x = if abs_line == s_line {
                        (inner.x + s_col).min(inner.x + inner.width)
                    } else {
                        inner.x
                    };
                    let to_x = if abs_line == e_line {
                        (inner.x + e_col).min(inner.x + inner.width)
                    } else {
                        inner.x + inner.width
                    };
                    for x in from_x..to_x {
                        buf[(x, y)].set_bg(Color::Rgb(50, 80, 135));
                        // Ensure text remains legible over the selection background.
                        if buf[(x, y)].fg == Color::Reset {
                            buf[(x, y)].set_fg(Color::Rgb(220, 220, 230));
                        }
                    }
                }
            }
        }

        // ── Action label overlays (no-nvim mode) ──────────────────────────────
        // Labels: [y] copy  [r] rerun  [e] edit  [x] delete
        if self.no_nvim && inner.width > 10 {
            let (icon_rerun, icon_edit, icon_delete, icon_copy) = if self.ascii {
                ("r", "e", "x", "y")
            } else {
                ("↻", "✎", "✕", "y")
            };

            let unavailable = Style::default().fg(Color::Rgb(50, 50, 65));
            let copy_style = Style::default().fg(Color::Rgb(100, 180, 220));
            let edit_active = Style::default().fg(Color::Rgb(200, 200, 210));
            let rerun_active = Style::default()
                .fg(Color::Rgb(80, 180, 120))
                .add_modifier(Modifier::DIM);
            let delete_style = Style::default()
                .fg(Color::Rgb(180, 70, 70))
                .add_modifier(Modifier::DIM);
            let confirm_style = Style::default()
                .fg(Color::Rgb(220, 180, 60))
                .add_modifier(Modifier::BOLD);

            // Layout: ... [y] [r] [e] [x]  — 2 chars each + 1 separator
            // Total: 9 chars from the right edge
            let x_copy = inner.x + inner.width.saturating_sub(9);
            let x_rerun = inner.x + inner.width.saturating_sub(7);
            let x_edit = inner.x + inner.width.saturating_sub(5);
            let x_delete = inner.x + inner.width.saturating_sub(3);

            for &abs_line in &self.labels.remove_label_lines {
                if abs_line < self.scroll_offset as usize {
                    continue;
                }
                let vis_row = abs_line - self.scroll_offset as usize;
                if vis_row >= content_height as usize {
                    continue;
                }
                let y = inner.y + vis_row as u16;

                if self.labels.pending_delete_line == Some(abs_line) {
                    buf.set_string(x_copy, y, "del?", confirm_style);
                } else {
                    let cs = if self.labels.copy_label_lines.contains(&abs_line) {
                        copy_style
                    } else {
                        unavailable
                    };
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
                    buf.set_string(x_copy, y, icon_copy, cs);
                    buf[(x_copy + 1, y)].set_symbol("");
                    buf.set_string(x_rerun, y, icon_rerun, rs);
                    buf[(x_rerun + 1, y)].set_symbol("");
                    buf.set_string(x_edit, y, icon_edit, es);
                    buf[(x_edit + 1, y)].set_symbol("");
                    buf.set_string(x_delete, y, icon_delete, delete_style);
                    buf[(x_delete + 1, y)].set_symbol("");
                }
            }
        }

        // ── Scrollbar (rightmost column) ──────────────────────────────────────
        if self.no_nvim && inner.width > 1 {
            let total_lines = self.lines.len();
            let visible_height = content_height as usize;
            if total_lines > visible_height {
                let sb_x = inner.x + inner.width - 1;
                let sb_area = Rect::new(sb_x, inner.y, 1, content_height);
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
        }

        // ── Auto-scroll paused banner ─────────────────────────────────────────
        if self.auto_scroll_paused && banner_reserved > 0 {
            let banner_y = inner.y + content_height;
            let msg = if self.ascii {
                "v  New content below -- press G to scroll to bottom  v"
            } else {
                "↓  New content below  ·  press G to jump to bottom  ↓"
            };
            let msg_chars: String = msg.chars().take(inner.width as usize).collect();
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
