// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cached layout metrics updated each frame and durable user-controlled split
//! size preferences.
//!
//! `SplitPrefs`  - durable, user-controlled split dimensions. Survives layout
//!                 recompute. Only the user can change these (drag or keys).
//! `LayoutCache` - transient, frame-derived rects and dimensions. Discardable;
//!                 rebuilt every loop iteration from `SplitPrefs`.

use ratatui::layout::Rect;

/// Which pane border is currently being dragged, with an anchor offset.
///
/// `anchor_offset` is the signed distance from the click coordinate to the
/// actual border coordinate at the moment `MouseDown` was received.  Applying
/// it during `MouseDrag` keeps the border locked to the cursor's original
/// grab point rather than jumping on first contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDrag {
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
    /// User-controlled input pane height preference (rows, including borders).
    /// Clamped to [3, 20] by the layout computation.
    pub input_height: u16,
}

impl SplitPrefs {
    pub fn new() -> Self {
        Self { input_height: 5 }
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
}

// ── LayoutCache ───────────────────────────────────────────────────────────────

/// Transient layout measurements cached from the previous rendered frame.
///
/// Populated at the top of the run-loop before any event processing so that
/// event handlers can query pane dimensions without needing a live frame
/// reference. Completely discardable - rebuilt each loop iteration from
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
    /// Active drag resize state - `Some` while the user holds down the mouse on a border.
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
            resize_drag: None,
        }
    }
}
