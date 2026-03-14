// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Queue panel widget — compact list of pending messages above the input box.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    prelude::StatefulWidget,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Widget},
};
use sven_config::AgentMode;

use super::theme::pane_block;
use super::width_utils::{display_width, truncate_to_width};

/// A single item in the queue panel.
pub struct QueueItem<'a> {
    pub content: &'a str,
    /// Model override label (e.g. `"anthropic/claude-opus-4-6"`).
    pub model_label: Option<&'a str>,
    pub mode_label: Option<AgentMode>,
}

/// Pending-message queue panel.
pub struct QueuePanel<'a> {
    pub items: &'a [QueueItem<'a>],
    pub selected: Option<usize>,
    pub editing: Option<usize>,
    pub focused: bool,
    pub ascii: bool,
}

impl Widget for QueuePanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || self.items.is_empty() {
            return;
        }
        let count = self.items.len();
        let title = format!(
            "Queue  [{count}]  [↑↓:select  e:edit  d:del  Enter/f:force-submit  s:submit-idle  Esc:close]"
        );
        let block = pane_block(&title, self.focused, self.ascii);
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 {
            return;
        }

        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = self.selected == Some(i);
                let is_editing = self.editing == Some(i);

                let num_span = Span::styled(
                    format!(" {} ", i + 1),
                    if is_selected || is_editing {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::LightBlue)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                );

                let badge: String = match (item.model_label, item.mode_label) {
                    (Some(m), Some(mo)) => format!("[{m}, {mo}] "),
                    (Some(m), None) => format!("[{m}] "),
                    (None, Some(mo)) => format!("[{mo}] "),
                    (None, None) => String::new(),
                };

                let badge_len = display_width(&badge);
                let max_text = inner.width.saturating_sub(6 + badge_len as u16) as usize;
                let first_line = item.content.lines().next().unwrap_or("");
                let needs_ellipsis =
                    display_width(first_line) > max_text || item.content.contains('\n');
                let preview = if needs_ellipsis {
                    truncate_to_width(first_line, max_text)
                } else {
                    first_line.to_string()
                };
                let text_content = format!(" {preview}");

                let text_span = Span::styled(
                    text_content,
                    if is_editing {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::ITALIC)
                    } else if is_selected {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                );

                let badge_span = if badge.is_empty() {
                    Span::raw("")
                } else {
                    Span::styled(badge, Style::default().fg(Color::Magenta))
                };

                ListItem::new(Line::from(vec![num_span, badge_span, text_span]))
            })
            .collect();

        // Use a transient ListState with no selection highlight — the styling
        // is already baked into each ListItem's Span styles (selected/editing).
        let list = List::new(list_items);
        let mut list_state = ListState::default();
        StatefulWidget::render(list, inner, buf, &mut list_state);
    }
}
