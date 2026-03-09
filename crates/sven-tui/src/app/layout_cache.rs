// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cached layout metrics updated each frame to avoid recomputing them inside
//! event handlers.

use ratatui::layout::Rect;

/// Minimum and maximum widths for the chat list pane.
pub const CHAT_LIST_MIN_WIDTH: u16 = 15;
pub const CHAT_LIST_MAX_WIDTH: u16 = 60;
pub const CHAT_LIST_DEFAULT_WIDTH: u16 = 32;

/// Which pane border is currently being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDrag {
    /// Dragging the vertical border between the main content and the chat list sidebar.
    /// Value stored is the column where the drag started (for delta calculation).
    ChatListWidth,
    /// Dragging the horizontal border between the chat/queue area and the input pane.
    InputHeight,
}

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
    /// Last known bounding rect of the chat list pane.
    pub chat_list_pane: Rect,
    /// User-controlled input pane height preference (rows, including borders).
    /// Clamped to [3, 20] by the layout computation.
    pub input_height_pref: u16,
    /// Preferred width of the right-side chat list pane.
    /// `0` means the pane is hidden.
    pub chat_list_width_pref: u16,
    /// Whether the chat list sidebar is currently visible.
    pub chat_list_visible: bool,
    /// Active drag resize state — `Some` while the user holds down the mouse on a border.
    pub resize_drag: Option<ResizeDrag>,
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
            chat_list_pane: Rect::default(),
            input_height_pref: 5,
            chat_list_width_pref: CHAT_LIST_DEFAULT_WIDTH,
            chat_list_visible: true,
            resize_drag: None,
        }
    }

    /// Effective width passed to `AppLayout::compute` — 0 when hidden.
    pub fn effective_chat_list_width(&self) -> u16 {
        if self.chat_list_visible {
            self.chat_list_width_pref
        } else {
            0
        }
    }

    /// Return `true` when `(col, row)` falls within the ±1 hit-zone of the
    /// vertical border separating the main content from the chat list sidebar.
    pub fn on_chat_list_border(&self, col: u16, _row: u16) -> bool {
        if !self.chat_list_visible || self.chat_list_pane.width == 0 {
            return false;
        }
        // The border is at the left edge of the chat list pane.
        let border_col = self.chat_list_pane.x;
        col >= border_col.saturating_sub(1) && col <= border_col + 1
    }

    /// Return `true` when `(col, row)` falls within the ±1 hit-zone of the
    /// horizontal border at the top of the input pane.
    pub fn on_input_border(&self, _col: u16, row: u16) -> bool {
        if self.input_pane.height == 0 {
            return false;
        }
        let border_row = self.input_pane.y;
        row >= border_row.saturating_sub(1) && row <= border_row
    }

    /// Update the chat list width while dragging.
    /// `col` is the current mouse column.
    pub fn drag_chat_list_width(&mut self, col: u16) {
        // Right edge of the terminal = chat_list_pane.x + chat_list_pane.width.
        let right_edge = self.chat_list_pane.x + self.chat_list_pane.width;
        let new_width = right_edge.saturating_sub(col);
        self.chat_list_width_pref = new_width.clamp(CHAT_LIST_MIN_WIDTH, CHAT_LIST_MAX_WIDTH);
    }

    /// Update the input pane height while dragging.
    /// `row` is the current mouse row.
    pub fn drag_input_height(&mut self, row: u16) {
        // Bottom edge of the input pane = input_pane.y + input_pane.height.
        let bottom_edge = self.input_pane.y + self.input_pane.height;
        let new_height = bottom_edge.saturating_sub(row);
        self.input_height_pref = new_height.clamp(3, 20);
    }

    /// Grow the chat list pane by 2 columns.
    pub fn chat_list_grow(&mut self) {
        self.chat_list_width_pref = (self.chat_list_width_pref + 2).min(CHAT_LIST_MAX_WIDTH);
    }

    /// Shrink the chat list pane by 2 columns.
    pub fn chat_list_shrink(&mut self) {
        self.chat_list_width_pref =
            (self.chat_list_width_pref.saturating_sub(2)).max(CHAT_LIST_MIN_WIDTH);
    }
}
