// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat segment types and helpers — shared logic is in `sven-frontend`.
//!
//! This module re-exports the canonical `ChatSegment` type and all pure
//! helpers from `sven_frontend::segment`. TUI-specific display helpers that
//! depend on terminal display-width utilities remain here.

// Re-export the canonical type and all framework-agnostic helpers.
pub use sven_frontend::{
    messages_for_resubmit, segment_at_line, segment_editable_text, segment_is_removable,
    segment_is_rerunnable, segment_tool_call_id, ChatSegment,
};

// ── TUI-specific display helper ───────────────────────────────────────────────

use crate::ui::width_utils::{display_width, truncate_to_width_exact};
use sven_model::MessageContent;

/// Return a short single-line preview of a segment for use in TUI dialog
/// messages and labels. Uses terminal display-width for accurate CJK truncation.
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
        Some(ChatSegment::TodoUpdate(todos)) => {
            return format!("(todo update · {} items)", todos.len())
        }
        Some(ChatSegment::CollabEvent(ev)) => return sven_core::prompts::format_collab_event(ev),
        Some(ChatSegment::DelegateSummary {
            to_name,
            task_title,
            status,
            ..
        }) => return format!("(delegated \"{task_title}\" to {to_name}: {status})"),
    };
    let first_line = raw.lines().next().unwrap_or("").trim();
    if display_width(first_line) > MAX {
        format!("\"{}…\"", truncate_to_width_exact(first_line, MAX))
    } else {
        format!("\"{first_line}\"")
    }
}
