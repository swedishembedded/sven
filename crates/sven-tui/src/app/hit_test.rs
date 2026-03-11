// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Mouse hit-testing: translate `(col, row)` into a typed [`HitArea`].
//!
//! This is the single source of truth for all pane boundary checks.  Every
//! mouse handler should call [`hit_test`] and pattern-match on the result
//! instead of replicating raw coordinate arithmetic.

use crate::app::layout_cache::LayoutCache;

// ── Area types ────────────────────────────────────────────────────────────────

/// The logical area of the TUI that a `(col, row)` coordinate falls into.
///
/// Returned by [`hit_test`]; callers pattern-match on this value and never
/// inspect raw coordinates again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitArea {
    /// A row inside the chat-list sidebar.
    ///
    /// `inner_row` is the 0-based visual row within the inner area (border
    /// excluded).  The scroll offset has **not** been applied; callers add
    /// `chat_list_scroll_offset()` to obtain the actual item index.
    ChatList { inner_row: usize },

    /// The scrollbar column of the chat pane.
    ///
    /// `rel_row` is 0-based from the top of the content area (first row after
    /// the top border).
    ChatScrollbar { rel_row: u16 },

    /// A click inside the chat content area (body text).
    ///
    /// `abs_line` already has `chat.scroll_offset` added, so it is an index
    /// into `chat.lines`.  `inner_col` is 0-based from the left edge of the
    /// chat pane (border excluded).
    ChatContent { abs_line: usize, inner_col: u16 },

    /// The input pane (any row / column).
    InputPane,

    /// A row inside the queue panel; `index` is the item index.
    QueueItem { index: usize },

    /// The vertical resize border between the chat-list and main content.
    ChatListBorder,

    /// The horizontal resize border between the chats and peers panes in the
    /// sidebar.
    PeersSplitBorder,

    /// The horizontal resize border above the input pane.
    InputBorder,

    /// Outside every defined pane.
    Outside,
}

// ── hit_test ──────────────────────────────────────────────────────────────────

/// Translate raw terminal coordinates into a [`HitArea`].
///
/// # Parameters
///
/// - `layout`   — cached pane rectangles for the current frame
/// - `col`,`row` — 0-based terminal coordinates from the mouse event
/// - `chat_scroll_offset` — current `chat.scroll_offset`
/// - `total_chat_lines`   — current `chat.lines.len()`
/// - `queue_len`      — number of items in `queue.messages`
pub fn hit_test(
    layout: &LayoutCache,
    col: u16,
    row: u16,
    chat_scroll_offset: u16,
    total_chat_lines: usize,
    queue_len: usize,
) -> HitArea {
    // ── Resize borders (checked before pane interiors so a drag that drifts ──
    // ── into a pane still registers as a border hit) ──────────────────────────

    // Vertical border: left edge of the chat list sidebar (±1 col hit-zone).
    let cl = layout.chat_list_pane;
    if cl.width > 0 {
        let border_col = cl.x;
        if col >= border_col.saturating_sub(1) && col <= border_col + 1 {
            return HitArea::ChatListBorder;
        }
    }

    // Horizontal border: top edge of the input pane (row-1..row hit-zone).
    let ip = layout.input_pane;
    if ip.height > 0 {
        let border_row = ip.y;
        if row >= border_row.saturating_sub(1) && row <= border_row {
            return HitArea::InputBorder;
        }
    }

    // Horizontal border: top edge of the peers pane within the sidebar
    // (±1 row hit-zone, constrained to sidebar columns).
    let pp = layout.peers_pane;
    if pp.height > 0 && cl.height > 0 {
        let border_row = pp.y;
        if row >= border_row.saturating_sub(1)
            && row <= border_row + 1
            && col >= cl.x
            && col < cl.x + cl.width
        {
            return HitArea::PeersSplitBorder;
        }
    }

    // ── Chat-list sidebar ─────────────────────────────────────────────────────
    if cl.width > 0 && col >= cl.x && col < cl.x + cl.width && row >= cl.y && row < cl.y + cl.height
    {
        let inner_row = (row as usize).saturating_sub((cl.y + 1) as usize);
        return HitArea::ChatList { inner_row };
    }

    // ── Input pane ────────────────────────────────────────────────────────────
    if row >= ip.y && row < ip.y + ip.height {
        return HitArea::InputPane;
    }

    // ── Queue panel ───────────────────────────────────────────────────────────
    let qp = layout.queue_pane;
    if qp.height > 0 && row >= qp.y && row < qp.y + qp.height {
        let inner_y = qp.y + 1; // skip top border
        if row >= inner_y {
            let item_idx = (row - inner_y) as usize;
            if item_idx < queue_len {
                return HitArea::QueueItem { index: item_idx };
            }
        }
        return HitArea::Outside;
    }

    // ── Chat pane ─────────────────────────────────────────────────────────────
    let cp = layout.chat_pane;
    let content_start = cp.y + 1; // skip top border
    let chat_inner_h = cp.height.saturating_sub(2);
    // The scrollbar occupies the rightmost column of the content area.
    let scrollbar_col = cp.x + cp.width.saturating_sub(1);
    let total_lines = total_chat_lines as u16;

    if row >= content_start && row < content_start + chat_inner_h {
        let rel_row = row - content_start;
        let abs_line = rel_row as usize + chat_scroll_offset as usize;

        // Scrollbar column (only visible when content overflows the pane)
        if col == scrollbar_col && chat_inner_h > 0 && total_lines > chat_inner_h {
            return HitArea::ChatScrollbar { rel_row };
        }

        // Content click (expand/collapse, selection anchor). Segment actions
        // (yank, edit, rerun, delete) are keyboard-first via y/e/r/x.
        let inner_col = col.saturating_sub(cp.x).min(cp.width.saturating_sub(1));
        return HitArea::ChatContent {
            abs_line,
            inner_col,
        };
    }

    HitArea::Outside
}
