// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Help overlay widget — centred modal listing all key bindings.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::theme::border_type;

/// Centred help overlay listing all key bindings.
pub struct HelpOverlay {
    pub ascii: bool,
}

impl Widget for HelpOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let bt = border_type(self.ascii);
        let help_text = vec![
            Line::from(Span::styled(
                "  Sven Key Bindings",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::LightBlue),
            )),
            Line::default(),
            Line::from(" ^w k     Focus chat pane"),
            Line::from(" ^w j     Focus input pane"),
            Line::from(" j/k/J/K  Scroll chat down/up"),
            Line::from(" ^u/^d    Half-page up/down"),
            Line::from(" g / G    Jump to top/bottom"),
            Line::from(" e / F2   Edit focused message (centre of chat view)"),
            Line::from(" x        Remove focused segment (+ paired tool result)"),
            Line::from(" r        Rerun from focused segment (truncate + re-run)"),
            Line::from(" d / F8   Truncate from focused message onward"),
            Line::from(" [Edit]   Click to load message into input box"),
            Line::from(" [x]      Click to remove this segment from history"),
            Line::from(" [r]      Click to rerun from this segment"),
            Line::from("           Live preview as you type; Enter submits"),
            Line::from("           Submitting discards later conversation"),
            Line::from("           Esc to cancel and restore original"),
            Line::from(" click    Click any message to collapse / expand"),
            Line::from("           (user, agent, tool calls, thinking blocks)"),
            Line::from(" Up/Down  Move cursor up/down a line in input box"),
            Line::from(" PgUp/Dn  Scroll input box by a page"),
            Line::from(" scroll   Mouse wheel over input scrolls input box"),
            Line::from(" ^T       Open full-screen pager"),
            Line::from(" /        Open search bar"),
            Line::from(" n / N    Next/prev search match"),
            Line::from(" Enter    Submit input (confirm edit in edit mode)"),
            Line::from(" S+Enter  Insert newline (^J if S+Enter not available)"),
            Line::from(" F4       Cycle agent mode"),
            Line::from(" ^c       Interrupt agent / quit"),
            Line::from(" F1       Toggle this help"),
            Line::default(),
            Line::from(Span::styled(
                " Press any key to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let width = 60u16.min(area.width);
        let height = (help_text.len() as u16 + 2).min(area.height);
        let x = area.width.saturating_sub(width) / 2;
        let y = area.height.saturating_sub(height) / 2;
        let overlay = Rect::new(x, y, width, height);

        Clear.render(overlay, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(bt)
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(overlay);
        block.render(overlay, buf);
        Paragraph::new(help_text).render(inner, buf);
    }
}
