// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
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
    /// Right-side chat list pane (zero-width when hidden).
    pub chat_list_pane: Rect,
    /// Right-side peers pane (zero-height when hidden or no peers).
    pub peers_pane: Rect,
}

impl AppLayout {
    /// Calculate layout regions from a `Rect` (terminal area).
    ///
    /// `queue_len`         — controls whether a queue panel is shown.
    /// `input_height`      — user-preferred input pane height (clamped 3–20).
    /// `chat_list_width`   — width of the right-side chat list pane (0 = hidden).
    /// `peers_pane_height` — height of the peers pane at the bottom of the sidebar (0 = hidden).
    pub fn compute(
        area: Rect,
        search_visible: bool,
        queue_len: usize,
        input_height: u16,
        chat_list_width: u16,
        peers_pane_height: u16,
    ) -> Self {
        let status_height = 1u16;
        let input_height = input_height.clamp(3, area.height.saturating_sub(5).max(3));
        let search_height = if search_visible { 1u16 } else { 0u16 };
        let queue_height: u16 = if queue_len > 0 {
            (queue_len as u16 + 2).min(6)
        } else {
            0
        };

        // ── Horizontal split: left (main) + right (chat list) ─────────────────
        let (main_area, chat_list_pane) = if chat_list_width > 0 {
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20), Constraint::Length(chat_list_width)])
                .split(area);
            (horiz[0], horiz[1])
        } else {
            (area, Rect::default())
        };

        // ── Vertical split within the sidebar: chats (top) + peers (bottom) ─────
        let (chat_list_pane, peers_pane) = if peers_pane_height > 0 && chat_list_width > 0 {
            let sidebar_area = Rect::new(
                chat_list_pane.x,
                chat_list_pane.y,
                chat_list_pane.x + chat_list_pane.width,
                chat_list_pane.y + chat_list_pane.height,
            );
            let vert = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(peers_pane_height)])
                .split(sidebar_area);
            (vert[0], vert[1])
        } else {
            (chat_list_pane, Rect::default())
        };

        // ── Vertical split within the main area ───────────────────────────────
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(status_height),
                Constraint::Min(10),
                Constraint::Length(queue_height),
                Constraint::Length(input_height),
                Constraint::Length(search_height),
            ])
            .split(main_area);

        AppLayout {
            status_bar: vertical[0],
            chat_pane: vertical[1],
            queue_pane: vertical[2],
            input_pane: vertical[3],
            search_bar: vertical[4],
            chat_list_pane,
            peers_pane,
        }
    }

    /// Convenience wrapper — derive the area from the current frame.
    pub fn new(
        frame: &Frame,
        search_visible: bool,
        queue_len: usize,
        input_height: u16,
        chat_list_width: u16,
        peers_pane_height: u16,
    ) -> Self {
        Self::compute(
            frame.area(),
            search_visible,
            queue_len,
            input_height,
            chat_list_width,
            peers_pane_height,
        )
    }

    /// The number of text rows visible inside the chat pane's border.
    /// With TOP|BOTTOM-only borders this is height minus 2 (one row each side).
    pub fn chat_inner_height(&self) -> u16 {
        self.chat_pane.height.saturating_sub(2)
    }
}
