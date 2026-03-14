// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Toast notification stack — brief ephemeral messages in the bottom-right corner.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use crate::app::ui_state::Toast;

use super::width_utils::{display_width, truncate_to_width_exact};

/// Renders up to 4 toasts stacked above the bottom-right corner of `area`.
pub struct ToastStack<'a> {
    pub toasts: &'a [Toast],
    pub ascii: bool,
}

impl Widget for ToastStack<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible: Vec<&Toast> = self
            .toasts
            .iter()
            .filter(|t| !t.is_expired())
            .take(4)
            .collect();

        if visible.is_empty() {
            return;
        }

        let max_len = visible
            .iter()
            .map(|t| display_width(&t.message))
            .max()
            .unwrap_or(10);
        // +4 for icon prefix + padding
        let width = (max_len as u16 + 5)
            .clamp(18, 52)
            .min(area.width.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(width);

        // Render from the bottom up so newest toast is at the bottom.
        let mut row = area.y + area.height.saturating_sub(1);
        for toast in visible.iter().rev() {
            if row < area.y || row > area.y + area.height {
                break;
            }
            let fg = toast.color;
            let check = if self.ascii { "* " } else { "\u{2713} " }; // ✓
            let msg = truncate_to_width_exact(&toast.message, (width as usize).saturating_sub(3));
            let text = format!("{check}{msg}");

            let toast_area = Rect::new(x, row, width, 1);
            Clear.render(toast_area, buf);
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            )))
            .render(toast_area, buf);

            row = row.saturating_sub(1);
        }
    }
}
