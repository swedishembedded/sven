// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::AgentMode;

/// The lifecycle state of a [`TodoItem`].
///
/// Serialises as the lowercase snake_case string the LLM expects
/// (`"pending"`, `"in_progress"`, `"completed"`, `"cancelled"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    /// Icon used in single-line todo summaries.
    pub fn icon(&self) -> &'static str {
        match self {
            TodoStatus::Completed => "✓",
            TodoStatus::InProgress => "→",
            TodoStatus::Cancelled => "✗",
            TodoStatus::Pending => "○",
        }
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

/// A structured todo item managed by the todo_write tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
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
    /// A delegate subtree has completed; emit a condensed summary in the chat.
    DelegateSummary {
        /// Name of the agent the work was delegated to.
        to_name: String,
        /// Short title of the delegated task.
        task_title: String,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
        /// `"completed"`, `"failed"`, or `"partial"`.
        status: String,
        /// First meaningful line of the result, shown collapsed.
        result_preview: String,
    },
    /// A subagent was started via the task tool; the TUI can create a child session.
    SubagentStarted {
        /// Tool-call ID for the spawn (matches `ToolCallStarted`).
        call_id: String,
        /// Buffer handle for the subagent output (e.g. `buf_0001`).
        handle_id: String,
        /// Short human-readable description for the sidebar.
        description: String,
    },
}
