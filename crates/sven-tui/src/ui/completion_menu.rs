// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash-command completion menu widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::overlay::completion::CompletionOverlay;

use super::theme::border_type;

/// Floating completion menu rendered above (or below) the input pane.
pub struct CompletionMenu<'a> {
    pub overlay: &'a CompletionOverlay,
    pub input_pane: Rect,
    pub ascii: bool,
}

impl Widget for CompletionMenu<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.overlay.items.is_empty() {
            return;
        }

        let visible = self.overlay.visible_items();
        let item_count = visible.len();

        let width = 70u16.min(self.input_pane.width.max(40));
        let height = (item_count as u16 + 2).min(area.height.saturating_sub(2));

        // Prefer above the input pane; fall back to below.
        let y = if self.input_pane.y >= height {
            self.input_pane.y - height
        } else {
            self.input_pane.y + self.input_pane.height
        };
        let x = self.input_pane.x;
        let menu_area = Rect::new(
            x.min(area.width.saturating_sub(width)),
            y.min(area.height.saturating_sub(height)),
            width,
            height,
        );

        Clear.render(menu_area, buf);

        let bt = border_type(self.ascii);
        let total = self.overlay.items.len();
        let scroll_indicator = if total > self.overlay.max_visible {
            format!(" [{}/{}]", self.overlay.selected + 1, total)
        } else {
            String::new()
        };

        let title = format!(" Commands{scroll_indicator} ");
        let block = Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(menu_area);
        block.render(menu_area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let max_val_width = (inner.width as usize).saturating_sub(2);
        let lines: Vec<Line<'static>> = visible
            .iter()
            .enumerate()
            .map(|(vis_idx, item)| {
                let actual_idx = self.overlay.scroll_offset + vis_idx;
                let is_selected = actual_idx == self.overlay.selected;

                let display = if item.display.is_empty() {
                    item.value.clone()
                } else {
                    item.display.clone()
                };
                let desc_str = item.description.as_deref().unwrap_or("");
                let sep = if desc_str.is_empty() { "" } else { "  " };
                let full = format!("{}{}{}", display, sep, desc_str);
                let truncated: String = full.chars().take(max_val_width).collect();

                if is_selected {
                    Line::from(Span::styled(
                        format!(" {truncated} "),
                        Style::default()
                            .bg(Color::Cyan)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    let disp_chars: String = display.chars().take(max_val_width).collect();
                    let remaining = max_val_width.saturating_sub(disp_chars.len());
                    if !desc_str.is_empty() && remaining > 3 {
                        let short_desc: String =
                            desc_str.chars().take(remaining.saturating_sub(2)).collect();
                        Line::from(vec![
                            Span::styled(
                                format!(" {disp_chars}"),
                                Style::default().fg(Color::White),
                            ),
                            Span::styled(
                                format!("  {short_desc}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ])
                    } else {
                        Line::from(Span::styled(
                            format!(" {disp_chars}"),
                            Style::default().fg(Color::Gray),
                        ))
                    }
                }
            })
            .collect();

        Paragraph::new(lines).render(inner, buf);
    }
}
