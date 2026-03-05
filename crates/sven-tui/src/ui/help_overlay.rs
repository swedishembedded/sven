// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Help overlay — two-column grid of key bindings, shown on F1.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::theme::{border_type, BAR_AGENT, BAR_TOOL, BG_ELEVATED, BORDER_FOCUS, TEXT, TEXT_DIM};

/// All key binding entries, grouped into sections.  Each tuple is `(key, description, is_header)`.
const BINDINGS: &[(&str, &str, bool)] = &[
    ("── Navigation ──", "", true),
    ("^w k / ^w ↑", "Focus chat pane", false),
    ("^w j / ^w ↓", "Focus input pane", false),
    ("^w + / ^w -", "Grow/shrink input pane", false),
    ("── Chat pane ──", "", true),
    ("j / k", "Scroll down/up", false),
    ("^d / ^u", "Page down / page up", false),
    ("g / G", "Scroll to top / bottom", false),
    ("/ n N", "Search / next / prev match", false),
    ("click / Enter", "Cycle expand level", false),
    ("e", "Edit message at cursor", false),
    ("y", "Copy segment to clipboard", false),
    ("Y", "Copy all to clipboard", false),
    ("x", "Remove segment", false),
    ("d", "Truncate chat from here", false),
    ("r", "Rerun from segment", false),
    ("^t", "Open pager", false),
    ("── Input pane ──", "", true),
    ("Enter", "Send message", false),
    ("Alt+Enter", "New line", false),
    ("^c", "Interrupt agent", false),
    ("^k / ^u", "Delete to end/start", false),
    ("^Up / ^Dn", "History older/newer", false),
    ("/ …", "Slash commands", false),
    ("── Queue panel ──", "", true),
    ("q / Esc", "Open/close queue", false),
    ("↑ ↓", "Navigate queue", false),
    ("e", "Edit selected message", false),
    ("Enter", "Force-submit selected", false),
    ("d / Del", "Delete selected", false),
    ("── General ──", "", true),
    ("F1", "Toggle this help", false),
    ("F4", "Cycle agent mode", false),
    ("Esc", "Cancel / close overlay", false),
];

pub struct HelpOverlay {
    pub ascii: bool,
}

impl Widget for HelpOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = 80u16.min(area.width.saturating_sub(4));
        let height = 34u16.min(area.height.saturating_sub(2));

        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        Clear.render(popup_area, buf);

        let bt = border_type(self.ascii);
        let block = Block::default()
            .title(Span::styled(
                "  Key bindings  (F1 or any key to close)  ",
                Style::default().fg(BAR_AGENT).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(BORDER_FOCUS))
            .style(Style::default().bg(BG_ELEVATED));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        if inner.width < 20 || inner.height < 4 {
            return;
        }

        // Split entries into two columns.
        let half = (BINDINGS.len() + 1) / 2;
        let (left_entries, right_entries) = BINDINGS.split_at(half.min(BINDINGS.len()));

        let [left_col, right_col] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner)[..]
        else {
            return;
        };

        render_column(left_entries, left_col, buf);
        render_column(right_entries, right_col, buf);
    }
}

fn render_column(entries: &[(&str, &str, bool)], area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let key_width = 18usize;
    let lines: Vec<Line<'static>> = entries
        .iter()
        .take(area.height as usize)
        .map(|(key, desc, is_header)| {
            if *is_header {
                Line::from(vec![Span::styled(
                    key.to_string(),
                    Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
                )])
            } else {
                let key_str: String = key.chars().take(key_width).collect();
                let desc_avail = (area.width as usize).saturating_sub(key_width + 1);
                let desc_str: String = desc.chars().take(desc_avail).collect();
                Line::from(vec![
                    Span::styled(
                        format!(" {key_str:<kw$} ", kw = key_width),
                        Style::default().fg(BAR_TOOL).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc_str, Style::default().fg(TEXT)),
                ])
            }
        })
        .collect();

    Paragraph::new(lines)
        .style(Style::default().bg(BG_ELEVATED))
        .render(area, buf);
}
