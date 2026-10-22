// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat segment types and helpers - shared logic is in `sven-frontend`.
//!
//! This module re-exports the canonical `ChatSegment` type and the pure
//! helpers the TUI uses from `sven_frontend::segment`.

// Re-export the canonical type and all framework-agnostic helpers.
pub use sven_frontend::{
    messages_for_resubmit, segment_editable_text, segment_tool_call_id,
    tool_result_insert_position, tool_runs, ChatSegment, ToolRun,
};
