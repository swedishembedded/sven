// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane state: conversation segments, rendered display lines, scroll, and labels.

use std::collections::{HashMap, HashSet};

use crate::{chat::segment::ChatSegment, markdown::StyledLines};

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
    /// Segments that are collapsed (ratatui-only mode).
    pub collapsed: HashSet<usize>,
    /// Absolute `lines` indices that carry an `[Edit]` label overlay.
    pub edit_labels: HashSet<usize>,
    /// Absolute `lines` indices that carry a `[x]` (remove) label overlay.
    pub remove_labels: HashSet<usize>,
    /// Absolute `lines` indices that carry a `[r]` (rerun) label overlay.
    pub rerun_labels: HashSet<usize>,
    /// The segment index closest to the vertical centre of the chat viewport.
    pub focused_segment: Option<usize>,
    /// `call_id → tool_name` lookup used when rendering tool results.
    pub tool_args: HashMap<String, String>,
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
            collapsed: HashSet::new(),
            edit_labels: HashSet::new(),
            remove_labels: HashSet::new(),
            rerun_labels: HashSet::new(),
            focused_segment: None,
            tool_args: HashMap::new(),
        }
    }
}
