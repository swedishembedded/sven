// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cached layout metrics updated each frame and durable user-controlled split
//! size preferences.
//!
//! `SplitPrefs`  — durable, user-controlled split dimensions. Survives layout
//!                 recompute. Only the user can change these (drag or keys).
//! `LayoutCache` — transient, frame-derived rects and dimensions. Discardable;
//!                 rebuilt every loop iteration from `SplitPrefs`.

use ratatui::layout::Rect;

/// Minimum and maximum widths for the chat list pane.
pub const CHAT_LIST_MIN_WIDTH: u16 = 15;
pub const CHAT_LIST_MAX_WIDTH: u16 = 60;
pub const CHAT_LIST_DEFAULT_WIDTH: u16 = 32;
/// Minimum and maximum heights for the peers pane.
pub const PEERS_PANE_MIN_HEIGHT: u16 = 5;
pub const PEERS_PANE_MAX_HEIGHT: u16 = 30;
pub const PEERS_PANE_DEFAULT_HEIGHT: u16 = 12;

/// Which pane border is currently being dragged, with an anchor offset.
///
/// `anchor_offset` is the signed distance from the click coordinate to the
/// actual border coordinate at the moment `MouseDown` was received.  Applying
/// it during `MouseDrag` keeps the border locked to the cursor's original
/// grab point rather than jumping on first contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDrag {
    /// Dragging the vertical border between the main content and the chat list sidebar.
    /// `anchor_offset` = click_col − border_col at start of drag.
    ChatListWidth { anchor_offset: i16 },
    /// Dragging the horizontal border between the chats and peers panes in the sidebar.
    /// `anchor_offset` = click_row − border_row at start of drag.
    PeersSplit { anchor_offset: i16 },
    /// Dragging the horizontal border between the chat/queue area and the input pane.
    /// `anchor_offset` = click_row − border_row at start of drag.
    InputHeight { anchor_offset: i16 },
}

// ── SplitPrefs ────────────────────────────────────────────────────────────────

/// Durable user-controlled split sizes.
///
/// These are the dimensions the user adjusts by dragging borders or pressing
/// resize keys. They survive layout recompute; only user actions change them.
pub(crate) struct SplitPrefs {
    /// Preferred width of the right-side chat list pane.
    pub chat_list_width: u16,
    /// Preferred height of the peers pane at the bottom of the sidebar.
    pub peers_pane_height: u16,
    /// User-controlled input pane height preference (rows, including borders).
    /// Clamped to [3, 20] by the layout computation.
    pub input_height: u16,
    /// Whether the chat list sidebar is currently visible.
    pub chat_list_visible: bool,
}

impl SplitPrefs {
    pub fn new() -> Self {
        Self {
            chat_list_width: CHAT_LIST_DEFAULT_WIDTH,
            peers_pane_height: PEERS_PANE_DEFAULT_HEIGHT,
            input_height: 5,
            chat_list_visible: true,
        }
    }

    /// Effective chat list width passed to `AppLayout::compute` — 0 when hidden.
    pub fn effective_chat_list_width(&self) -> u16 {
        if self.chat_list_visible {
            self.chat_list_width
        } else {
            0
        }
    }

    /// Effective peers pane height passed to `AppLayout::compute` — 0 when hidden.
    pub fn effective_peers_pane_height(&self) -> u16 {
        if self.chat_list_visible && self.peers_pane_height > 0 {
            self.peers_pane_height
        } else {
            0
        }
    }

    /// Update the chat list width while dragging.
    ///
    /// `col` is the current mouse column; `anchor` is the offset recorded on
    /// `MouseDown` (`click_col − border_col`).
    pub fn drag_chat_list_width(&mut self, col: u16, anchor: i16, layout: &LayoutCache) {
        let adjusted = (col as i16 - anchor).max(0) as u16;
        let right_edge = layout.chat_list_pane.x + layout.chat_list_pane.width;
        let new_width = right_edge.saturating_sub(adjusted);
        self.chat_list_width = new_width.clamp(CHAT_LIST_MIN_WIDTH, CHAT_LIST_MAX_WIDTH);
    }

    /// Update the peers pane height while dragging.
    ///
    /// `row` is the current mouse row; `anchor` is the offset recorded on
    /// `MouseDown` (`click_row − border_row`).
    pub fn drag_peers_pane_height(&mut self, row: u16, anchor: i16, layout: &LayoutCache) {
        let adjusted = (row as i16 - anchor).max(0) as u16;
        // `chat_list_pane` is only the upper (Chats) half of the sidebar after
        // the split, so its bottom edge equals the border row, not the sidebar
        // bottom.  Use the peers pane's own bottom edge instead.
        let sidebar_bottom = layout.peers_pane.y + layout.peers_pane.height;
        let new_height = sidebar_bottom.saturating_sub(adjusted);
        self.peers_pane_height = new_height.clamp(PEERS_PANE_MIN_HEIGHT, PEERS_PANE_MAX_HEIGHT);
    }

    /// Update the input pane height while dragging.
    ///
    /// `row` is the current mouse row; `anchor` is the offset recorded on
    /// `MouseDown` (`click_row − border_row`).
    pub fn drag_input_height(&mut self, row: u16, anchor: i16, layout: &LayoutCache) {
        let adjusted = (row as i16 - anchor).max(0) as u16;
        let bottom_edge = layout.input_pane.y + layout.input_pane.height;
        let new_height = bottom_edge.saturating_sub(adjusted);
        self.input_height = new_height.clamp(3, 20);
    }

    /// Grow the chat list pane by 2 columns.
    pub fn chat_list_grow(&mut self) {
        self.chat_list_width = (self.chat_list_width + 2).min(CHAT_LIST_MAX_WIDTH);
    }

    /// Shrink the chat list pane by 2 columns.
    pub fn chat_list_shrink(&mut self) {
        self.chat_list_width = (self.chat_list_width.saturating_sub(2)).max(CHAT_LIST_MIN_WIDTH);
    }
}

// ── LayoutCache ───────────────────────────────────────────────────────────────

/// Transient layout measurements cached from the previous rendered frame.
///
/// Populated at the top of the run-loop before any event processing so that
/// event handlers can query pane dimensions without needing a live frame
/// reference. Completely discardable — rebuilt each loop iteration from
/// `SplitPrefs`.
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
    /// Last known bounding rect of the peers pane.
    pub peers_pane: Rect,
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
            peers_pane: Rect::default(),
            resize_drag: None,
        }
    }
}
