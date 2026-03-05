// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Which-key popup — shown briefly after a Ctrl+w prefix is pressed,
//! listing all available follow-up keys. Inspired by LazyVim / which-key.nvim.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::theme::border_type;

/// Floating which-key popup.
pub struct WhichKeyOverlay {
    pub ascii: bool,
}

impl Widget for WhichKeyOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: &[(&str, &str)] = &[
            ("k / ↑", "Focus chat pane"),
            ("j / ↓", "Focus input pane"),
            ("+  /  =", "Grow input pane"),
            ("-", "Shrink input pane"),
            ("Esc", "Cancel"),
        ];

        let width = 36u16.min(area.width.saturating_sub(4));
        let height = (items.len() as u16 + 3).min(area.height.saturating_sub(2));

        // Position in the top-right, just below the status bar.
        let x = area.width.saturating_sub(width + 2);
        let y = 1u16;
        let popup_area = Rect::new(x, y, width, height);

        Clear.render(popup_area, buf);

        let bt = border_type(self.ascii);
        let block = Block::default()
            .title(Span::styled(
                " ^w … ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let lines: Vec<Line> = items
            .iter()
            .map(|(key, desc)| {
                Line::from(vec![
                    Span::styled(
                        format!(" {:<9}", key),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", desc), Style::default().fg(Color::White)),
                ])
            })
            .collect();

        Paragraph::new(lines).render(inner, buf);
    }
}
