// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash-command completion menu widget — floating popup above the input pane
//! with slash prefix and a one-row description preview.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget},
};

use crate::overlay::completion::CompletionOverlay;

use super::theme::border_type;
use super::width_utils::truncate_to_width_exact;

// ── CompletionMenu widget ─────────────────────────────────────────────────────

/// Floating completion overlay — positioned just above the input pane.
pub struct CompletionMenu<'a> {
    pub overlay: &'a mut CompletionOverlay,
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

        // ── Item list via ratatui List + ListState ────────────────────────────
        let avail = (inner.width as usize).saturating_sub(1);
        let items: Vec<ListItem> = self
            .overlay
            .items
            .iter()
            .map(|item| {
                let raw_label = if item.display.is_empty() {
                    item.value.as_str()
                } else {
                    item.display.as_str()
                };
                let label = truncate_to_width_exact(raw_label, avail);
                ListItem::new(Line::from(Span::raw(label)))
            })
            .collect();

        let list = List::new(items)
            .style(Style::default().bg(Color::Black))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            );

        StatefulWidget::render(list, list_area, buf, &mut self.overlay.list_state);

        // ── Description preview ───────────────────────────────────────────────
        if let Some(item) = self.overlay.selected_item() {
            let desc = item.description.as_deref().unwrap_or("");
            if !desc.is_empty() {
                let avail_desc = inner.width.saturating_sub(2) as usize;
                let desc_str = truncate_to_width_exact(desc, avail_desc);
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
