// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// The regions that make up the TUI layout.
#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    pub status_bar: Rect,
    pub chat_pane: Rect,
    /// Activity panel under the chat; zero-height without subagents.
    pub activity_pane: Rect,
    /// Queue panel shown above the input pane; zero-height when queue is empty.
    pub queue_pane: Rect,
    pub input_pane: Rect,
    pub search_bar: Rect,
}

/// How tall the panes other than the chat want to be.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaneSizes {
    pub search_visible: bool,
    /// Messages waiting in the queue; the queue panel shows when there are any.
    pub queue_len: usize,
    /// Rows for the activity panel (0 hides it).
    pub activity_height: u16,
    /// Input pane height (clamped 3-20).
    pub input_height: u16,
}

impl AppLayout {
    /// Calculate layout regions from a `Rect` (terminal area): the status
    /// bar, the chat, the activity and queue panels, the input and the
    /// search bar, top to bottom. The chat gets what the others leave.
    ///
    /// There is no sidebar: the chat pane always spans the full terminal
    /// width, with no controls on either side. Session switching and peers
    /// are reached through `/resume` and `/peers` instead of a permanent
    /// side panel.
    pub fn compute(area: Rect, sizes: PaneSizes) -> Self {
        let status_height = 1u16;
        let input_height = sizes
            .input_height
            .clamp(3, area.height.saturating_sub(5).max(3));
        let search_height = u16::from(sizes.search_visible);
        let queue_height: u16 = if sizes.queue_len > 0 {
            (sizes.queue_len as u16 + 2).min(6)
        } else {
            0
        };

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(status_height),
                Constraint::Min(10),
                Constraint::Length(sizes.activity_height),
                Constraint::Length(queue_height),
                Constraint::Length(input_height),
                Constraint::Length(search_height),
            ])
            .split(area);

        AppLayout {
            status_bar: vertical[0],
            chat_pane: vertical[1],
            activity_pane: vertical[2],
            queue_pane: vertical[3],
            input_pane: vertical[4],
            search_bar: vertical[5],
        }
    }

    /// The number of text rows visible inside the chat pane's border.
    /// With TOP|BOTTOM-only borders this is height minus 2 (one row each side).
    pub fn chat_inner_height(&self) -> u16 {
        self.chat_pane.height.saturating_sub(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chat pane must span the entire terminal width - no columns held
    /// back for a sidebar, a gutter, or a scrollbar.
    #[test]
    fn chat_pane_spans_the_full_terminal_width() {
        for (w, h) in [(40, 20), (80, 24), (120, 40), (200, 60)] {
            let area = Rect::new(0, 0, w, h);
            let layout = AppLayout::compute(
                area,
                PaneSizes {
                    input_height: 4,
                    ..PaneSizes::default()
                },
            );
            assert_eq!(
                layout.chat_pane.width, w,
                "chat_pane width must equal the terminal width at {w}x{h}"
            );
            assert_eq!(layout.chat_pane.x, 0);
        }
    }
}
