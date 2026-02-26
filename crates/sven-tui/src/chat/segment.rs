// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Core chat data: the `ChatSegment` enum and helpers that operate on segment
//! slices without needing access to the full `App` state.

use sven_core::CompactionStrategyUsed;
use sven_model::{Message, MessageContent, Role};

/// One entry in the chat display (a concrete message or a display-only note).
#[derive(Debug, Clone)]
pub enum ChatSegment {
    Message(Message),
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
        strategy: CompactionStrategyUsed,
        turn: u32,
    },
    Error(String),
    Thinking { content: String },
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
        _ => None,
    }
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
