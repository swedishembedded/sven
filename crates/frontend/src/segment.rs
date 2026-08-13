// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Re-exports [`sven_session_model`]'s `ChatSegment` and segment-slice
//! helpers. Moved there so TUI and CI can share the same pure "fold a
//! `SessionEvent` stream into a displayable conversation" model without
//! depending on `sven-frontend`'s tokio-based agent-wiring plumbing.

pub use sven_session_model::{
    format_collab_event, messages_for_resubmit, segment_at_line, segment_editable_text,
    segment_is_removable, segment_is_rerunnable, segment_short_preview, segment_tool_call_id,
    tool_result_insert_position, ChatSegment,
};
