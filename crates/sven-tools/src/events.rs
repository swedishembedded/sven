// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::AgentMode;

/// A structured todo item managed by the todo_write tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    /// One of: "pending", "in_progress", "completed", "cancelled"
    pub status: String,
}

/// Events emitted by tools to communicate state changes back to the agent loop.
/// The agent translates these into `AgentEvent` variants for the UI.
#[derive(Debug)]
pub enum ToolEvent {
    TodoUpdate(Vec<TodoItem>),
    ModeChanged(AgentMode),
    /// Real-time progress update from a long-running tool.
    /// Forwarded immediately to the UI so the spinner reflects current activity.
    Progress {
        /// The tool-call ID this progress belongs to (matches `ToolCall::id`).
        call_id: String,
        /// Short human-readable status message, e.g. "chunk 12/200".
        message: String,
    },
}
