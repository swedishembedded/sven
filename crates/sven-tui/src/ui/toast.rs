// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Toast notification stack — brief ephemeral messages in the bottom-right corner.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Widget,
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
        let x = area.width.saturating_sub(width);

        // Render from the bottom up so newest toast is at the bottom.
        let mut row = area.height.saturating_sub(1);
        for toast in visible.iter().rev() {
            if row == 0 || row > area.height {
                break;
            }
            let fg = toast.color;
            let check = if self.ascii { "* " } else { "✓ " };
            let msg = truncate_to_width_exact(&toast.message, (width as usize).saturating_sub(3));
            let text = format!("{check}{msg}");

            // Clear the background so the toast doesn't bleed into underlying content.
            for col in x..x + width {
                buf[(col, row)].reset();
            }

            buf.set_string(
                x,
                row,
                &text,
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            );

            row = row.saturating_sub(1);
        }
    }
}
