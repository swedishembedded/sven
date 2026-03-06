// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash-command completion menu widget — floating popup above the input pane
//! with slash prefix and a one-row description preview.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::overlay::completion::CompletionOverlay;

use super::theme::border_type;

// ── CompletionMenu widget ─────────────────────────────────────────────────────

/// Floating completion overlay — positioned just above the input pane.
pub struct CompletionMenu<'a> {
    pub overlay: &'a CompletionOverlay,
    /// The input pane rect — used to anchor the popup above it.
    pub input_pane: Rect,
    pub ascii: bool,
}

impl Widget for CompletionMenu<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.overlay.items.is_empty() {
            return;
        }

        let list_h = self.overlay.items.len().min(8) as u16;
        // +1 description row + 2 borders = 3 overhead rows
        let popup_height = (list_h + 3).min(area.height.saturating_sub(2));
        let popup_width = 52u16.min(area.width.saturating_sub(2)).max(24);

        // Anchor the popup so its bottom edge is just above the input pane top.
        let bottom = self.input_pane.y;
        let y = bottom.saturating_sub(popup_height);
        let x = self
            .input_pane
            .x
            .min(area.width.saturating_sub(popup_width));

        let popup_area = Rect::new(x, y, popup_width, popup_height);
        if popup_area.height < 3 {
            return;
        }

        Clear.render(popup_area, buf);

        let bt = border_type(self.ascii);
        let block = Block::default()
            .title(Span::styled(" Commands ", Style::default().fg(Color::Cyan)))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        if inner.height < 2 {
            return;
        }

        // Split inner area: list takes all rows except the last (description).
        let list_rows = inner.height.saturating_sub(1);
        let list_area = Rect::new(inner.x, inner.y, inner.width, list_rows);
        let desc_area = Rect::new(inner.x, inner.y + list_rows, inner.width, 1);

        // ── Item list ─────────────────────────────────────────────────────────
        let scroll = self.overlay.scroll_offset;
        let mut lines: Vec<Line<'static>> = Vec::new();

        for (i, item) in self
            .overlay
            .items
            .iter()
            .skip(scroll)
            .take(list_rows as usize)
            .enumerate()
        {
            let abs_idx = i + scroll;
            let selected = abs_idx == self.overlay.selected;

            let raw_label = if item.display.is_empty() {
                item.value.as_str()
            } else {
                item.display.as_str()
            };
            let label = raw_label;
            let avail = (inner.width as usize).saturating_sub(1);
            let label: String = label.chars().take(avail).collect();

            let (fg, bg, modifier) = if selected {
                (Color::Black, Color::LightCyan, Modifier::BOLD)
            } else {
                (Color::White, Color::Black, Modifier::empty())
            };

            let base = Style::default().fg(fg).bg(bg).add_modifier(modifier);

            // Pad to fill the row so bg covers the full width.
            let pad_len = (inner.width as usize).saturating_sub(label.len() + 1);
            let pad = " ".repeat(pad_len);

            lines.push(Line::from(vec![
                Span::styled(format!(""), base),
                Span::styled(format!("{label}{pad}"), base),
            ]));
        }

        Paragraph::new(lines)
            .style(Style::default().bg(Color::Black))
            .render(list_area, buf);

        // ── Description preview ───────────────────────────────────────────────
        if let Some(item) = self.overlay.items.get(self.overlay.selected) {
            let desc = item.description.as_deref().unwrap_or("");
            if !desc.is_empty() {
                let avail = inner.width.saturating_sub(2) as usize;
                let desc_str: String = desc.chars().take(avail).collect();
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        desc_str,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]))
                .render(desc_area, buf);
            }
        }
    }
}
