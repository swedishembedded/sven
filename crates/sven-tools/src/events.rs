// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use serde_json::Value;
use sven_config::AgentMode;

/// A structured event streamed from a subagent over ACP.
///
/// This is a sven-native mirror of ACP `SessionUpdate` variants, kept
/// dependency-free so `sven-tools` does not need to depend on the ACP crate.
#[derive(Debug, Clone)]
pub enum SubagentUpdate {
    /// A chunk of assistant text (streamed).
    TextDelta(String),
    /// A chunk of thinking/reasoning text (streamed).
    ThinkingDelta(String),
    /// The subagent started a tool call.
    ToolCallStarted {
        id: String,
        name: String,
        args: Value,
    },
    /// A subagent tool call completed.
    ToolCallFinished {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// The subagent's turn is complete; `final_text` is the accumulated
    /// assistant response that the parent agent should use as the task result.
    Finished { final_text: String },
    /// The subagent timed out or terminated with an error.
    Failed { reason: String },
}

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

/// A structured todo item managed by the todo tool.
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
    /// The active model should change for subsequent turns.
    /// The string is a resolved `"provider/id"` identifier
    /// (e.g. `"anthropic/claude-opus-4-6"`).
    ModelChanged(String),
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
        /// Full prompt text sent to the subagent; shown as the first user message.
        prompt: String,
    },
    /// A structured event from a running subagent, streamed over ACP.
    /// The TUI uses these to build a proper conversation view for the subagent session.
    SubagentEvent {
        /// Tool-call ID of the spawning `task` call (matches `ToolCallStarted`).
        call_id: String,
        /// Buffer handle identifying which subagent session this belongs to.
        handle_id: String,
        /// The structured event payload.
        update: SubagentUpdate,
    },
}
