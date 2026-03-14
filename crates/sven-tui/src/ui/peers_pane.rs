// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Right-side peers pane widget.
//!
//! Displays currently connected peers when sven is running in P2P mode
//! with delegation support.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Widget},
};

use super::theme::{BORDER_DIM, BORDER_FOCUS, BORDER_RESIZE, TEXT, TEXT_DIM};
use super::width_utils::{fit_to_width, truncate_to_width};

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

        // ── Empty state ───────────────────────────────────────────────────────
        if self.items.is_empty() {
            let empty_msg = if self.ascii {
                "(no peers)"
            } else {
                " (no peers) "
            };
            Paragraph::new(Span::styled(empty_msg, Style::default().fg(TEXT_DIM)))
                .centered()
                .render(inner, buf);
            return;
        }

        // ── Build ListItems ───────────────────────────────────────────────────
        // Layout per row:  icon(1) + space(1) + name(…) + delegate_indicator(2)
        // The delegate indicator occupies the last 2 columns of the row.
        // We use fit_to_width to pad the name so the delegate column is always
        // at the same position, and then append the indicator at the end.
        let delegate_w: usize = 2; // " ⚡" / " D"
        let name_cols = (inner.width as usize).saturating_sub(2 + delegate_w); // 2 = icon + space

        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(item_idx, item)| {
                let is_cursor = item_idx == self.selected;

                // Status icon
                let (icon_char, icon_fg) = if item.connected {
                    (
                        if self.ascii { '*' } else { '\u{25CF}' }, // ●
                        Color::Rgb(100, 180, 100),
                    )
                } else {
                    (
                        if self.ascii { '-' } else { '\u{25CB}' }, // ○
                        TEXT_DIM,
                    )
                };

                // Row background
                let (bg_color, text_fg, text_mod) = if is_cursor {
                    (Color::Rgb(40, 40, 60), TEXT, Modifier::BOLD)
                } else {
                    (Color::Reset, TEXT_DIM, Modifier::empty())
                };

                // Name, padded/truncated to fixed width so delegate column aligns.
                let name_trunc = truncate_to_width(item.name, name_cols);
                let name_padded = fit_to_width(&name_trunc, name_cols);

                // Delegate indicator (right-aligned, always 2 cols).
                let delegate_str = if item.can_delegate {
                    if self.ascii {
                        " D"
                    } else {
                        " \u{26A1}"
                    } // ⚡
                } else {
                    "  "
                };
                let delegate_fg = if item.can_delegate {
                    Color::Rgb(255, 200, 100)
                } else {
                    Color::Reset
                };

                let line = Line::from(vec![
                    Span::styled(icon_char.to_string(), Style::default().fg(icon_fg)),
                    Span::raw(" "),
                    Span::styled(
                        name_padded,
                        Style::default().fg(text_fg).add_modifier(text_mod),
                    ),
                    Span::styled(delegate_str, Style::default().fg(delegate_fg)),
                ]);

                ListItem::new(line).style(Style::default().bg(bg_color))
            })
            .collect();

        // ── Render via ratatui List + transient ListState ─────────────────────
        let list = List::new(list_items);
        let mut list_state = ListState::default();
        *list_state.offset_mut() = self.scroll_offset;
        list_state.select(Some(self.selected));
        StatefulWidget::render(list, inner, buf, &mut list_state);
    }
}
