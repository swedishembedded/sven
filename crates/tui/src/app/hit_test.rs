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
/// - `layout`   - cached pane rectangles for the current frame
/// - `col`,`row` - 0-based terminal coordinates from the mouse event
/// - `chat_scroll_offset` - current `chat.scroll_offset`
/// - `queue_len`      - number of items in `queue.messages`
pub fn hit_test(
    layout: &LayoutCache,
    col: u16,
    row: u16,
    chat_scroll_offset: u16,
    queue_len: usize,
) -> HitArea {
    // ── Resize borders (checked before pane interiors so a drag that drifts ──
    // ── into a pane still registers as a border hit) ──────────────────────────

    // Horizontal border: top edge of the input pane (row-1..row hit-zone).
    let ip = layout.input_pane;
    if ip.height > 0 {
        let border_row = ip.y;
        if row >= border_row.saturating_sub(1) && row <= border_row {
            return HitArea::InputBorder;
        }
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

    if row >= content_start && row < content_start + chat_inner_h {
        let rel_row = row - content_start;
        let abs_line = rel_row as usize + chat_scroll_offset as usize;

        // Content click (expand/collapse, selection anchor). Segment actions
        // (yank, edit, rerun, delete) are keyboard-first via y/e/r/x. No
        // scrollbar column to special-case any more - the pane has no
        // visible scroll affordance (see `ui::chat_pane`'s module docs).
        let inner_col = col.saturating_sub(cp.x).min(cp.width.saturating_sub(1));
        return HitArea::ChatContent {
            abs_line,
            inner_col,
        };
    }

    HitArea::Outside
}
