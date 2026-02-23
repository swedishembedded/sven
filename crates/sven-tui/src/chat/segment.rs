// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Core chat data: the `ChatSegment` enum and helpers that operate on segment
//! slices without needing access to the full `App` state.

use sven_model::{Message, MessageContent, Role};

/// One entry in the chat display (a concrete message or a display-only note).
#[derive(Debug, Clone)]
pub enum ChatSegment {
    Message(Message),
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
    },
    Error(String),
    Thinking { content: String },
    /// A user message that has been queued while the agent is busy.
    /// Visible in the chat pane with a "pending" style; promoted to
    /// `Message(Message::user(...))` when the agent picks it up.
    Queued(String),
}

/// Return the segment index whose line range contains `line`, or `None` when
/// the line is inside the streaming-buffer area (no corresponding segment).
pub fn segment_at_line(
    segment_line_ranges: &[(usize, usize)],
    line: usize,
) -> Option<usize> {
    segment_line_ranges
        .iter()
        .position(|&(start, end)| line >= start && line < end)
}

/// If the segment at index `i` is an editable user or assistant text message,
/// return a clone of its text.  Returns `None` for tool calls, results, etc.
pub fn segment_editable_text(segments: &[ChatSegment], i: usize) -> Option<String> {
    let seg = segments.get(i)?;
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::User, MessageContent::Text(t))      => Some(t.clone()),
            (Role::Assistant, MessageContent::Text(t)) => Some(t.clone()),
            _ => None,
        },
        ChatSegment::Queued(t) => Some(t.clone()),
        _ => None,
    }
}

/// Returns `true` if the segment at index `i` is a `Queued` entry.
fn segment_is_queued(segments: &[ChatSegment], i: usize) -> bool {
    matches!(segments.get(i), Some(ChatSegment::Queued(_)))
}

/// Returns the position within the `VecDeque<String>` queue that corresponds
/// to the `Queued` segment at index `i`, i.e. the number of `Queued` segments
/// that appear *before* index `i`.
pub fn queued_deque_index(segments: &[ChatSegment], i: usize) -> Option<usize> {
    if !segment_is_queued(segments, i) {
        return None;
    }
    let pos = segments[..i]
        .iter()
        .filter(|s| matches!(s, ChatSegment::Queued(_)))
        .count();
    Some(pos)
}

/// Collect the `Message` objects from a segment slice, skipping non-message
/// entries (ContextCompacted, Error, Thinking).  Used when building the
/// payload for a Resubmit request.
pub fn messages_for_resubmit(segments: &[ChatSegment]) -> Vec<Message> {
    segments
        .iter()
        .filter_map(|s| match s {
            ChatSegment::Message(m) => Some(m.clone()),
            _ => None,
        })
        .collect()
}
