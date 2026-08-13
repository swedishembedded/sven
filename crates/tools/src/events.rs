// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::{AgentMode, McpServerConfig};
pub use sven_vocab::{SubagentUpdate, TodoItem, TodoStatus};

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
    /// An MCP server was added or re-enabled via the `system` tool.
    ///
    /// The TUI / agent loop should connect the new server and re-register its
    /// tools with the `ToolRegistry`.
    McpServerAdded {
        /// The server name (used as tool prefix, e.g. `"github"`).
        name: String,
        /// Full server configuration.
        config: McpServerConfig,
    },
    /// An MCP server was removed or disabled via the `system` tool.
    ///
    /// The TUI / agent loop should disconnect the server and unregister its
    /// tools from the `ToolRegistry`.
    McpServerRemoved(String),
}
