// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Mouse hit-testing: translate `(col, row)` into a typed [`HitArea`].
//!
//! This is the single source of truth for all pane boundary checks.  Every
//! mouse handler should call [`hit_test`] and pattern-match on the result
//! instead of replicating raw coordinate arithmetic.

use std::collections::HashSet;

use crate::{app::layout_cache::LayoutCache, chat::segment::segment_at_line};

// ── Area types ────────────────────────────────────────────────────────────────

/// Which right-aligned action icon on a segment header line was clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentIconAction {
    Copy,
    Rerun,
    Edit,
    Delete,
}

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

    /// One of the four right-aligned action icons on a segment header line.
    ChatSegmentIcon {
        seg_idx: usize,
        action: SegmentIconAction,
    },

    /// Any other click inside the chat content area (body text, not icon).
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
/// - `segment_line_ranges` — `chat.segment_line_ranges` slice
/// - `remove_labels`  — `chat.remove_labels` (lines that have a delete icon)
/// - `copy_labels`    — `chat.copy_labels`
/// - `edit_labels`    — `chat.edit_labels`
/// - `rerun_labels`   — `chat.rerun_labels`
/// - `queue_len`      — number of items in `queue.messages`
#[allow(clippy::too_many_arguments)]
pub fn hit_test(
    layout: &LayoutCache,
    col: u16,
    row: u16,
    chat_scroll_offset: u16,
    total_chat_lines: usize,
    segment_line_ranges: &[(usize, usize)],
    remove_labels: &HashSet<usize>,
    copy_labels: &HashSet<usize>,
    edit_labels: &HashSet<usize>,
    rerun_labels: &HashSet<usize>,
    queue_len: usize,
) -> HitArea {
    // ── Resize borders (checked before pane interiors so a drag that drifts ──
    // ── into a pane still registers as a border hit) ──────────────────────────
    if layout.on_chat_list_border(col, row) {
        return HitArea::ChatListBorder;
    }
    if layout.on_input_border(col, row) {
        return HitArea::InputBorder;
    }

    // ── Chat-list sidebar ─────────────────────────────────────────────────────
    let cl = layout.chat_list_pane;
    if layout.chat_list_visible
        && cl.width > 0
        && col >= cl.x
        && col < cl.x + cl.width
        && row >= cl.y
        && row < cl.y + cl.height
    {
        let inner_row = (row as usize).saturating_sub((cl.y + 1) as usize);
        return HitArea::ChatList { inner_row };
    }

    // ── Input pane ────────────────────────────────────────────────────────────
    let ip = layout.input_pane;
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

        // Right-aligned segment action icons on header lines.
        // Layout (9 cols from inner right, scrollbar at col w-1):
        //   [y] copy   — cols w-9 … w-7  (zone [w-9, w-7))
        //   [↻] rerun  — cols w-7 … w-5  (zone [w-7, w-5))
        //   [✎] edit   — cols w-5 … w-3  (zone [w-5, w-3))
        //   [✕] delete — cols w-3 … w-1  (zone [w-3, w-1))
        if col != scrollbar_col && remove_labels.contains(&abs_line) {
            let w = cp.width;
            let label_start = cp.x + w.saturating_sub(9);
            let rerun_start = cp.x + w.saturating_sub(7);
            let edit_start = cp.x + w.saturating_sub(5);
            let delete_start = cp.x + w.saturating_sub(3);
            let delete_end = cp.x + w.saturating_sub(1); // exclusive; scrollbar is at w-1

            if col >= label_start {
                if let Some(seg_idx) = segment_at_line(segment_line_ranges, abs_line) {
                    let action = if col >= delete_start && col < delete_end {
                        Some(SegmentIconAction::Delete)
                    } else if col >= edit_start
                        && col < delete_start
                        && edit_labels.contains(&abs_line)
                    {
                        Some(SegmentIconAction::Edit)
                    } else if col >= rerun_start
                        && col < edit_start
                        && rerun_labels.contains(&abs_line)
                    {
                        Some(SegmentIconAction::Rerun)
                    } else if col >= label_start
                        && col < rerun_start
                        && copy_labels.contains(&abs_line)
                    {
                        Some(SegmentIconAction::Copy)
                    } else {
                        None
                    };

                    if let Some(action) = action {
                        return HitArea::ChatSegmentIcon { seg_idx, action };
                    }
                }
            }
        }

        // Regular content click (body text, expand/collapse, selection anchor)
        let inner_col = col.saturating_sub(cp.x).min(cp.width.saturating_sub(1));
        return HitArea::ChatContent {
            abs_line,
            inner_col,
        };
    }

    HitArea::Outside
}
