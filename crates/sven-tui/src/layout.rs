// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

/// The regions that make up the TUI layout.
#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    pub status_bar: Rect,
    pub chat_pane: Rect,
    /// Queue panel shown above the input pane; zero-height when queue is empty.
    pub queue_pane: Rect,
    pub input_pane: Rect,
    pub search_bar: Rect,
}

impl AppLayout {
    /// Calculate layout regions from a `Rect` (terminal area).
    ///
    /// `queue_len` controls whether a queue panel is shown between the chat and
    /// input panes.  When zero the `queue_pane` rect is empty (height 0).
    pub fn compute(area: Rect, search_visible: bool, queue_len: usize) -> Self {
        let status_height = 1u16;
        let input_height = 5u16;
        let search_height = if search_visible { 1u16 } else { 0u16 };
        // Queue pane: 1 border row + 1 row per queued message (capped at 4), + 1 border row.
        // Hidden (height 0) when there are no queued messages.
        let queue_height: u16 = if queue_len > 0 {
            (queue_len as u16 + 2).min(6)
        } else {
            0
        };

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(status_height),
                Constraint::Min(10),
                Constraint::Length(queue_height),
                Constraint::Length(input_height),
                Constraint::Length(search_height),
            ])
            .split(area);

        AppLayout {
            status_bar: vertical[0],
            chat_pane: vertical[1],
            queue_pane: vertical[2],
            input_pane: vertical[3],
            search_bar: vertical[4],
        }
    }

    /// Convenience wrapper — derive the area from the current frame.
    pub fn new(frame: &Frame, search_visible: bool, queue_len: usize) -> Self {
        Self::compute(frame.area(), search_visible, queue_len)
    }

    /// The number of text rows visible inside the chat pane's border.
    /// (pane height minus the two border rows)
    pub fn chat_inner_height(&self) -> u16 {
        self.chat_pane.height.saturating_sub(2)
    }
}
