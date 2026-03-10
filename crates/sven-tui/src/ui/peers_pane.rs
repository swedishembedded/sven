// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Right-side peers pane widget.
//!
//! Displays currently connected peers when sven is running in P2P mode
//! with delegation support.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Widget},
};

use super::theme::{BORDER_DIM, BORDER_FOCUS, BORDER_RESIZE, TEXT, TEXT_DIM};
use super::width_utils::{char_width, display_width, truncate_to_width_exact};

/// Data for a single peer in the peers list.
pub struct PeerListItem<'a> {
    /// Peer display name.
    pub name: &'a str,
    /// Whether this peer is currently connected/active.
    pub connected: bool,
    /// Whether this peer has delegation enabled.
    pub can_delegate: bool,
}

/// Right-side peers pane widget.
///
/// Renders a scrollable list of connected peers with status indicators.
pub struct PeersPane<'a> {
    pub items: &'a [PeerListItem<'a>],
    /// Index of the keyboard-focused row.
    pub selected: usize,
    /// Whether this pane has keyboard focus.
    pub focused: bool,
    /// ASCII-only mode (no Unicode box-drawing characters).
    pub ascii: bool,
    /// Scroll offset (first visible row index).
    pub scroll_offset: usize,
    /// Whether the top border is currently being drag-resized.
    pub is_resizing: bool,
}

impl Widget for PeersPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // ── Border ────────────────────────────────────────────────────────────
        let border_style = if self.is_resizing {
            Style::default().fg(BORDER_RESIZE)
        } else if self.focused {
            Style::default().fg(BORDER_FOCUS)
        } else {
            Style::default().fg(BORDER_DIM)
        };
        let block = Block::default()
            .title(Span::styled(
                " Peers ",
                if self.focused {
                    Style::default()
                        .fg(BORDER_FOCUS)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TEXT_DIM)
                },
            ))
            .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
            .border_style(border_style)
            .border_type(if self.ascii {
                BorderType::Plain
            } else {
                BorderType::Rounded
            });

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width < 3 {
            return;
        }

        let visible_rows = inner.height as usize;
        let total = self.items.len();

        // Clamp scroll offset so selected is always visible.
        let scroll = self.scroll_offset;

        let visible_range = scroll..(scroll + visible_rows).min(total);

        for (row_idx, item_idx) in visible_range.enumerate() {
            let item = &self.items[item_idx];
            let y = inner.y + row_idx as u16;
            if y >= inner.y + inner.height {
                break;
            }

            let is_cursor = item_idx == self.selected;

            // ── Status icon ───────────────────────────────────────────────────
            let (icon_char, icon_style) = if item.connected {
                (
                    if self.ascii { '*' } else { '●' },
                    Style::default().fg(Color::Rgb(100, 180, 100)),
                )
            } else {
                (
                    if self.ascii { '-' } else { '○' },
                    Style::default().fg(TEXT_DIM),
                )
            };

            // ── Row background ───────────────────────────────────────────────
            let (bg_color, text_style) = if is_cursor {
                (
                    Color::Rgb(40, 40, 60),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )
            } else {
                (Color::Reset, Style::default().fg(TEXT_DIM))
            };

            // Fill background for the full row width.
            for x in 0..inner.width {
                let cell = buf.cell_mut((inner.x + x, y));
                if let Some(c) = cell {
                    c.set_bg(bg_color);
                }
            }

            // ── Icon ──────────────────────────────────────────────────────────
            let icon_x = inner.x;
            if let Some(cell) = buf.cell_mut((icon_x, y)) {
                cell.set_char(icon_char);
                cell.set_style(icon_style.bg(bg_color));
            }

            // ── Name (truncated) ─────────────────────────────────────────────
            let name_x = inner.x + 2;
            let max_name_width = inner.width.saturating_sub(2) as usize;

            let (display_name, needs_ellipsis) =
                if display_width(item.name) > max_name_width && max_name_width > 1 {
                    (
                        truncate_to_width_exact(item.name, max_name_width.saturating_sub(1)),
                        true,
                    )
                } else {
                    (truncate_to_width_exact(item.name, max_name_width), false)
                };

            let mut col_offset = 0u16;
            for ch in display_name.chars() {
                if let Some(cell) = buf.cell_mut((name_x + col_offset, y)) {
                    cell.set_char(ch);
                    cell.set_style(text_style.bg(bg_color));
                }
                col_offset += char_width(ch) as u16;
            }

            if needs_ellipsis {
                if let Some(cell) = buf.cell_mut((name_x + col_offset, y)) {
                    cell.set_char(if self.ascii { '.' } else { '…' });
                    cell.set_style(text_style.bg(bg_color));
                }
            }

            // ── Delegate indicator ───────────────────────────────────────────
            if item.can_delegate {
                let delegate_x = inner.x + inner.width.saturating_sub(2);
                if let Some(cell) = buf.cell_mut((delegate_x, y)) {
                    cell.set_char(if self.ascii { 'D' } else { '⚡' });
                    cell.set_style(Style::default().fg(Color::Rgb(255, 200, 100)).bg(bg_color));
                }
            }
        }

        // ── Empty state ───────────────────────────────────────────────────────
        if self.items.is_empty() {
            let empty_msg = if self.ascii {
                "(no peers)"
            } else {
                " (no peers) "
            };
            let msg_len = empty_msg.len() as u16;
            let x = inner.x + inner.width.saturating_sub(msg_len) / 2;
            let y = inner.y + inner.height / 2;
            for (i, ch) in empty_msg.chars().enumerate() {
                if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(TEXT_DIM));
                }
            }
        }
    }
}
