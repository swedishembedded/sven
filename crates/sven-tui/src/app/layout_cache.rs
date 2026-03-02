// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cached layout metrics updated each frame to avoid recomputing them inside
//! event handlers.

use ratatui::layout::Rect;

/// Layout measurements cached from the previous rendered frame.
///
/// Populated at the top of the run-loop before any event processing so that
/// event handlers can query pane dimensions without needing a live frame
/// reference.
pub(crate) struct LayoutCache {
    /// Number of content rows visible inside the chat pane border.
    pub chat_height: u16,
    /// Inner width of the chat pane (sans border).
    pub chat_inner_width: u16,
    /// Inner width of the input pane (sans border).
    pub input_inner_width: u16,
    /// Inner height of the input pane (sans border).
    pub input_inner_height: u16,
    /// Last known bounding rect of the entire chat pane (including border).
    pub chat_pane: Rect,
    /// Last known bounding rect of the entire input pane (including border).
    pub input_pane: Rect,
    /// Last known bounding rect of the queue panel.
    pub queue_pane: Rect,
}

impl LayoutCache {
    pub fn new() -> Self {
        Self {
            chat_height: 24,
            chat_inner_width: 78,
            input_inner_width: 78,
            input_inner_height: 3,
            chat_pane: Rect::default(),
            input_pane: Rect::default(),
            queue_pane: Rect::default(),
        }
    }
}
