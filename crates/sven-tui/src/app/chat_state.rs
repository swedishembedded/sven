// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane state: conversation segments, rendered display lines, scroll, and labels.

use std::collections::{HashMap, HashSet};

use crate::{chat::segment::ChatSegment, markdown::StyledLines};

/// Expand level for a collapsible segment.
///
/// - `0` — one-line summary (default for tool calls, tool results, thinking)
/// - `1` — partial view (first ~10 lines of content)
/// - `2` — full content (default for user text, agent text)
pub type ExpandLevel = u8;

/// All state owned by the chat pane.
pub(crate) struct ChatState {
    /// Rendered display lines (pre-wrapped, styled) used by the chat widget.
    pub lines: StyledLines,
    /// Authoritative conversation history (source of truth for display and resubmit).
    pub segments: Vec<ChatSegment>,
    /// Accumulated assistant text during streaming (until `TextComplete`).
    pub streaming_buffer: String,
    /// True while receiving `ThinkingDelta` events; controls thinking-block style.
    pub streaming_is_thinking: bool,
    /// `(start_line, end_line)` in `lines` for each segment in `segments`.
    /// Rebuilt whenever `build_display_from_segments` runs.
    pub segment_line_ranges: Vec<(usize, usize)>,
    /// Current scroll position (index of the topmost visible line).
    pub scroll_offset: u16,
    /// When true, new agent content automatically scrolls to the bottom.
    pub auto_scroll: bool,
    /// Per-segment expand level (ratatui-only mode).
    /// Segments not in this map use the default level for their type.
    pub expand_level: HashMap<usize, ExpandLevel>,
    /// Absolute `lines` indices that carry an `[Edit]` label overlay.
    pub edit_labels: HashSet<usize>,
    /// Absolute `lines` indices that carry a `[x]` (remove) label overlay.
    pub remove_labels: HashSet<usize>,
    /// Absolute `lines` indices that carry a `[r]` (rerun) label overlay.
    pub rerun_labels: HashSet<usize>,
    /// Absolute `lines` indices that carry a `[y]` (copy) label overlay.
    pub copy_labels: HashSet<usize>,
    /// The segment index closest to the vertical centre of the chat viewport.
    pub focused_segment: Option<usize>,
    /// `call_id → tool_name` lookup used when rendering tool results.
    pub tool_args: HashMap<String, String>,
    /// `call_id → elapsed_secs` for completed tool calls.
    pub tool_durations: HashMap<String, f32>,

    // ── Mouse text selection ──────────────────────────────────────────────────
    /// Drag-selection anchor: `(abs_line, col_from_inner_x)` set on mouse-down.
    pub selection_anchor: Option<(usize, u16)>,
    /// Drag-selection current end: `(abs_line, col_from_inner_x)` updated on drag.
    pub selection_end: Option<(usize, u16)>,
    /// True while or after a drag selection is active (cleared on next mouse-down).
    pub is_selecting: bool,
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            segments: Vec::new(),
            streaming_buffer: String::new(),
            streaming_is_thinking: false,
            segment_line_ranges: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            expand_level: HashMap::new(),
            edit_labels: HashSet::new(),
            remove_labels: HashSet::new(),
            rerun_labels: HashSet::new(),
            copy_labels: HashSet::new(),
            focused_segment: None,
            tool_args: HashMap::new(),
            tool_durations: HashMap::new(),
            selection_anchor: None,
            selection_end: None,
            is_selecting: false,
        }
    }

    /// Return the selection as a normalized `(start_abs_line, start_col, end_abs_line, end_col)`
    /// tuple where start ≤ end.  Returns `None` if no complete selection exists.
    pub fn normalized_selection(&self) -> Option<(usize, u16, usize, u16)> {
        let (a_line, a_col) = self.selection_anchor?;
        let (e_line, e_col) = self.selection_end?;
        if a_line < e_line || (a_line == e_line && a_col <= e_col) {
            Some((a_line, a_col, e_line, e_col))
        } else {
            Some((e_line, e_col, a_line, a_col))
        }
    }

    /// Return the effective expand level for segment `idx`.
    pub fn effective_expand_level(&self, idx: usize, seg: &ChatSegment) -> ExpandLevel {
        if let Some(&level) = self.expand_level.get(&idx) {
            return level;
        }
        default_expand_level(seg)
    }
}

/// Default expand level for a segment type.
/// - Tool calls, tool results, thinking → 0 (summary)
/// - User text, agent text → 2 (full)
pub fn default_expand_level(seg: &ChatSegment) -> ExpandLevel {
    use crate::chat::segment::ChatSegment;
    use sven_model::{MessageContent, Role};
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::User, MessageContent::Text(_)) | (Role::Assistant, MessageContent::Text(_)) => 2,
            _ => 0,
        },
        ChatSegment::Thinking { .. } => 0,
        _ => 2,
    }
}
