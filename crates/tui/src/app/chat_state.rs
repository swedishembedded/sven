// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat pane state: conversation segments, rendered display lines, scroll, and labels.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::{
    chat::segment::{tool_result_insert_position, ChatSegment},
    markdown::StyledLines,
};

/// Identity of a segment within one chat. It stays with the segment when
/// segments are inserted or removed around it, which an index does not.
pub type SegmentId = u64;

/// Expand level for a collapsible segment.
///
/// - `0` - one-line summary (default for tool calls, tool results, thinking)
/// - `1` - expanded, at most one chat pane tall; the rest opens in a viewer
/// - `2` - full content (default for user text, agent text)
pub type ExpandLevel = u8;

/// What the chat's keyboard highlight is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatFocus {
    /// A segment: a message, a tool, a note.
    Segment(usize),
    /// The "Ran N tools" header of the run of tools starting at this segment.
    ToolRun(usize),
}

impl ChatFocus {
    /// The segment the highlight is on, or the first one of its run. Actions
    /// on "the highlighted entry" (edit, copy, truncate) apply to it.
    pub fn index(self) -> usize {
        match self {
            ChatFocus::Segment(i) | ChatFocus::ToolRun(i) => i,
        }
    }

    fn with_index(self, i: usize) -> Self {
        match self {
            ChatFocus::Segment(_) => ChatFocus::Segment(i),
            ChatFocus::ToolRun(_) => ChatFocus::ToolRun(i),
        }
    }
}

/// All state owned by the chat pane.
#[derive(Clone)]
pub(crate) struct ChatState {
    /// Rendered display lines (pre-wrapped, styled) used by the chat widget.
    pub lines: StyledLines,
    /// Authoritative conversation history (source of truth for display and resubmit).
    /// Structural changes go through the `*_segment(s)` methods, which keep
    /// `segment_ids`, the expand state and the highlight attached to the
    /// segments they belong to.
    segments: Vec<ChatSegment>,
    /// `segment_ids[i]` is the identity of `segments[i]`.
    segment_ids: Vec<SegmentId>,
    /// The id the next added segment gets; ids are never reused.
    next_segment_id: SegmentId,
    /// Accumulated assistant text during streaming (until `TextComplete`).
    pub streaming_buffer: String,
    /// True while receiving `ThinkingDelta` events; controls thinking-block style.
    pub streaming_is_thinking: bool,
    /// `(start_line, end_line)` in `lines` for each segment in `segments`.
    /// Rebuilt whenever `build_display_from_segments` runs.
    pub segment_line_ranges: Vec<(usize, usize)>,
    /// Current scroll position (index of the topmost visible line).
    pub scroll_offset: usize,
    /// When true, new agent content automatically scrolls to the bottom.
    pub auto_scroll: bool,
    /// Per-segment expand level (ratatui-only mode), keyed by segment id.
    /// Segments not in this map use the default level for their type.
    expand_level: HashMap<SegmentId, ExpandLevel>,
    /// Runs of tools the user expanded, keyed by the run's first segment.
    expanded_runs: HashSet<SegmentId>,
    /// What the keyboard highlight is on. Inserting or removing a segment
    /// before it moves it along with its segment.
    pub focus: Option<ChatFocus>,
    /// Everything the reader can put the highlight on, in display order,
    /// with its `(start_line, end_line)`. Rebuilt with `lines`.
    pub view_items: Vec<(ChatFocus, (usize, usize))>,
    /// `call_id → tool_name` lookup used when rendering tool results.
    pub tool_args: HashMap<String, String>,
    /// `call_id → elapsed_secs` for completed tool calls.
    pub tool_durations: HashMap<String, f32>,
    /// `call_id → when it started`, for every tool started in this process;
    /// with the durations it gives a run of tools its wall-clock time.
    pub tool_started_at: HashMap<String, Instant>,
    /// `call_id → streaming preview` for in-progress tool calls that send
    /// incremental output via `ToolEvent::Progress`.  Used by the expanded
    /// (tier 1/2) view to show live output from sub-agent processes.
    pub tool_streaming_content: HashMap<String, String>,

    // ── Mouse text selection ──────────────────────────────────────────────────
    /// Drag-selection anchor: `(abs_line, col_from_inner_x)` set on mouse-down.
    pub selection_anchor: Option<(usize, u16)>,
    /// Drag-selection current end: `(abs_line, col_from_inner_x)` updated on drag.
    pub selection_end: Option<(usize, u16)>,
    /// True while or after a drag selection is active (cleared on next mouse-down).
    pub is_selecting: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            segments: Vec::new(),
            segment_ids: Vec::new(),
            next_segment_id: 0,
            streaming_buffer: String::new(),
            streaming_is_thinking: false,
            segment_line_ranges: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            expand_level: HashMap::new(),
            expanded_runs: HashSet::new(),
            focus: None,
            view_items: Vec::new(),
            tool_args: HashMap::new(),
            tool_durations: HashMap::new(),
            tool_started_at: HashMap::new(),
            tool_streaming_content: HashMap::new(),
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

    // ── Segments ──────────────────────────────────────────────────────────────

    /// The conversation, in display order.
    pub fn segments(&self) -> &[ChatSegment] {
        &self.segments
    }

    /// Mutable access to the segments' content. A slice cannot change the
    /// number or order of segments, so their identities are unchanged.
    pub fn segments_mut(&mut self) -> &mut [ChatSegment] {
        &mut self.segments
    }

    /// The stable id of the segment at `idx`.
    pub fn segment_id(&self, idx: usize) -> Option<SegmentId> {
        self.segment_ids.get(idx).copied()
    }

    fn fresh_id(&mut self) -> SegmentId {
        let id = self.next_segment_id;
        self.next_segment_id += 1;
        id
    }

    /// Append a segment and return its index.
    pub fn push_segment(&mut self, seg: ChatSegment) -> usize {
        let id = self.fresh_id();
        self.segments.push(seg);
        self.segment_ids.push(id);
        self.segments.len() - 1
    }

    /// Insert a segment at `idx`, shifting the segments after it.
    pub fn insert_segment(&mut self, idx: usize, seg: ChatSegment) {
        let id = self.fresh_id();
        self.segments.insert(idx, seg);
        self.segment_ids.insert(idx, id);
        if let Some(f) = self.focus {
            if f.index() >= idx {
                self.focus = Some(f.with_index(f.index() + 1));
            }
        }
    }

    /// Remove and return the segment at `idx`.
    pub fn remove_segment(&mut self, idx: usize) -> ChatSegment {
        let id = self.segment_ids.remove(idx);
        self.expand_level.remove(&id);
        self.expanded_runs.remove(&id);
        self.focus = match self.focus {
            Some(f) if f.index() == idx => None,
            Some(f) if f.index() > idx => Some(f.with_index(f.index() - 1)),
            other => other,
        };
        self.segments.remove(idx)
    }

    /// Keep only the first `len` segments.
    pub fn truncate_segments(&mut self, len: usize) {
        for id in self.segment_ids.drain(len.min(self.segments.len())..) {
            self.expand_level.remove(&id);
            self.expanded_runs.remove(&id);
        }
        self.segments.truncate(len);
        if self.focus.is_some_and(|f| f.index() >= len) {
            self.focus = None;
        }
    }

    /// Replace the whole conversation. The new segments are new identities:
    /// no expand state or highlight carries over from the old ones.
    pub fn set_segments(&mut self, segments: Vec<ChatSegment>) {
        self.segments.clear();
        self.segment_ids.clear();
        self.expand_level.clear();
        self.expanded_runs.clear();
        self.focus = None;
        for seg in segments {
            self.push_segment(seg);
        }
    }

    /// Add a tool's result right after the call it answers (or at the end
    /// when that call is not in this chat), and return its index. Errors are
    /// recorded with the `error: ` prefix the renderers key on.
    pub fn insert_tool_result(&mut self, call_id: &str, output: &str, is_error: bool) -> usize {
        let content = if is_error {
            format!("error: {output}")
        } else {
            output.to_string()
        };
        let seg = ChatSegment::Message(sven_model::Message::tool_result(call_id, &content));
        match tool_result_insert_position(&self.segments, call_id) {
            Some(pos) => {
                self.insert_segment(pos, seg);
                pos
            }
            None => self.push_segment(seg),
        }
    }

    /// The highlighted segment, or the first segment of the highlighted run.
    pub fn focused_segment(&self) -> Option<usize> {
        self.focus.map(ChatFocus::index)
    }

    /// Put the highlight on the segment at `idx`.
    pub fn focus_segment(&mut self, idx: usize) {
        self.focus = Some(ChatFocus::Segment(idx));
    }

    // ── Expansion ─────────────────────────────────────────────────────────────

    /// Whether the run of tools starting at segment `first` is expanded.
    pub fn run_expanded(&self, first: usize) -> bool {
        self.segment_id(first)
            .is_some_and(|id| self.expanded_runs.contains(&id))
    }

    /// Expand or collapse the run of tools starting at segment `first`.
    pub fn set_run_expanded(&mut self, first: usize, expanded: bool) {
        if let Some(id) = self.segment_id(first) {
            if expanded {
                self.expanded_runs.insert(id);
            } else {
                self.expanded_runs.remove(&id);
            }
        }
    }

    /// The expand level of the segment at `idx`: the one set for it, or the
    /// default for its kind.
    pub fn expand_level(&self, idx: usize) -> ExpandLevel {
        let set = self
            .segment_id(idx)
            .and_then(|id| self.expand_level.get(&id).copied());
        match (set, self.segments.get(idx)) {
            (Some(level), _) => level,
            (None, Some(seg)) => default_expand_level(seg),
            (None, None) => 0,
        }
    }

    /// Set the expand level of the segment at `idx`.
    pub fn set_expand_level(&mut self, idx: usize, level: ExpandLevel) {
        if let Some(id) = self.segment_id(idx) {
            self.expand_level.insert(id, level);
        }
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
