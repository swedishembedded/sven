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

/// Returns `true` when the segment can be spliced out of the conversation
/// (user text, assistant text/tool-call, tool result, thinking block).
/// `ContextCompacted` and `Error` entries are not user-removable.
pub fn segment_is_removable(seg: &ChatSegment) -> bool {
    match seg {
        ChatSegment::Message(_) | ChatSegment::Thinking { .. } => true,
        ChatSegment::ContextCompacted { .. } | ChatSegment::Error(_) => false,
    }
}

/// Returns `true` when the segment supports the "rerun from here" action
/// (assistant text, assistant tool calls, or tool results).
pub fn segment_is_rerunnable(seg: &ChatSegment) -> bool {
    match seg {
        ChatSegment::Message(m) => matches!(
            (&m.role, &m.content),
            (sven_model::Role::Assistant, _)
                | (sven_model::Role::Tool, MessageContent::ToolResult { .. })
        ),
        _ => false,
    }
}

/// If this segment is an assistant `ToolCall`, return its `tool_call_id`.
pub fn segment_tool_call_id(seg: &ChatSegment) -> Option<&str> {
    match seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::ToolCall { tool_call_id, .. } => Some(tool_call_id.as_str()),
            MessageContent::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// Return a short single-line preview of a segment for use in dialog messages.
pub fn segment_short_preview(seg: Option<&ChatSegment>) -> String {
    const MAX: usize = 60;
    let raw = match seg {
        None => return "(unknown)".into(),
        Some(ChatSegment::Message(m)) => match &m.content {
            MessageContent::Text(t) => t.trim().to_string(),
            MessageContent::ToolCall { function, .. } => format!("tool call: {}", function.name),
            MessageContent::ToolResult { content, .. } => content.to_string(),
            MessageContent::ContentParts(_) => "(multipart message)".into(),
        },
        Some(ChatSegment::Thinking { content }) => content.trim().to_string(),
        Some(ChatSegment::ContextCompacted { .. }) => return "(context compaction)".into(),
        Some(ChatSegment::Error(e)) => e.trim().to_string(),
    };
    // Collapse to first line and truncate.
    let first_line = raw.lines().next().unwrap_or("").trim();
    if first_line.chars().count() > MAX {
        format!("\"{}…\"", first_line.chars().take(MAX).collect::<String>())
    } else {
        format!("\"{first_line}\"")
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
