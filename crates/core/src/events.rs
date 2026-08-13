// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::AgentMode;
use sven_tools::{
    events::{SubagentUpdate, TodoItem},
    ToolCall,
};

pub use sven_vocab::{CompactionStrategyUsed, PeerInfo};

/// Events emitted by the agent during a single turn.
/// Consumers (CI runner, TUI) subscribe to these to drive their output.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A text chunk streamed from the model
    TextDelta(String),
    /// A complete text response from the model (after streaming finishes)
    TextComplete(String),
    /// A thinking/reasoning chunk from the model (extended thinking API).
    /// Consumers should accumulate deltas and finalise them into a Thinking
    /// segment when the model signals the end of the reasoning block.
    ThinkingDelta(String),
    /// A complete thinking/reasoning block (accumulated from ThinkingDelta events).
    ThinkingComplete(String),
    /// The model has requested a tool call
    ToolCallStarted(ToolCall),
    /// A tool call finished
    ToolCallFinished {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    /// Context was compacted; statistics for the UI.
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
        /// Which compaction strategy was used.
        strategy: CompactionStrategyUsed,
        /// Agentic loop round in which compaction fired (0 = pre-submit).
        turn: u32,
    },
    /// Current token usage update.
    ///
    /// Providers may emit this multiple times per turn with different fields
    /// populated (e.g. Anthropic sends input stats on `message_start` and
    /// output stats on `message_delta`).  Fields that were not reported for
    /// this particular event are zero; consumers should only update their
    /// display when the relevant field is non-zero.
    TokenUsage {
        /// Input tokens processed this request (does NOT include cache hits).
        input: u32,
        /// Output tokens generated this request.
        output: u32,
        /// Tokens served from the provider's prompt cache this turn.
        cache_read: u32,
        /// Tokens written into the provider's prompt cache this turn.
        cache_write: u32,
        /// Running total of cache-read tokens across the whole session.
        cache_read_total: u32,
        /// Running total of cache-write tokens across the whole session.
        cache_write_total: u32,
        /// The model's maximum context window (tokens).  Zero means unknown.
        max_tokens: usize,
        /// The model's maximum output tokens per completion.  Zero means unknown.
        /// See `sven_model::budget::effective_input_budget` for how this and
        /// `max_tokens` combine into a usable input budget.
        max_output_tokens: usize,
        /// Cost in USD when reported by the API (e.g. OpenRouter).
        cost_usd: Option<f64>,
    },
    /// The agent has finished processing the current user turn
    TurnComplete,
    /// The current run was aborted (via Ctrl+C or /abort).
    /// `partial_text` contains any assistant text that was streamed before the
    /// abort; it may be empty if the model had not yet produced any output.
    /// The agent has committed `partial_text` (when non-empty) to its session
    /// history so a follow-up Resubmit will see it.
    Aborted { partial_text: String },
    /// A recoverable error occurred
    Error(String),
    /// A long-running tool is reporting incremental progress.
    /// The UI should update the spinner / status bar without adding a chat segment.
    ToolProgress {
        /// The tool-call ID this update belongs to (matches `ToolCallStarted`).
        call_id: String,
        /// Short human-readable status, e.g. "context_query: chunk 12/200".
        message: String,
    },
    /// The todo list was updated
    TodoUpdate(Vec<TodoItem>),
    /// The agent mode was changed
    ModeChanged(AgentMode),
    /// The active model was changed by the agent tool.
    /// The string is a resolved `"provider/id"` identifier
    /// (e.g. `"anthropic/claude-opus-4-6"`).
    /// The TUI/CI runner should apply this for subsequent submissions.
    ModelChanged(String),
    /// The agent is asking the user a question (id links to QuestionAnswer)
    Question { id: String, questions: Vec<String> },
    /// Answer to a previous Question event
    QuestionAnswer { id: String, answer: String },
    /// Chat title generated from the first user message (LLM, low max_tokens).
    /// The TUI sets the session title as soon as this is received.
    TitleGenerated(String),
    /// A team lifecycle event to be shown in the chat as a collapsible segment.
    CollabEvent(crate::prompts::CollabEvent),
    /// A completed delegate subtree - rendered as a collapsible `DelegateSummary` segment.
    DelegateSummary {
        to_name: String,
        task_title: String,
        duration_ms: u64,
        status: String,
        result_preview: String,
    },
    /// A subagent was started via the task tool; the TUI creates a child session.
    SubagentStarted {
        call_id: String,
        handle_id: String,
        description: String,
        /// Full prompt sent to the subagent; shown as the first user message in its view.
        prompt: String,
    },
    /// A structured ACP event streamed from a running subagent.
    /// The TUI uses these to build a proper conversation view for the subagent session.
    SubagentEvent {
        /// Tool-call ID of the spawning `task` call (matches `ToolCallStarted`).
        call_id: String,
        /// Buffer handle identifying which subagent session this belongs to.
        handle_id: String,
        /// The structured event payload.
        update: SubagentUpdate,
    },
    /// List of peers (from node proxy / list_peers).
    PeerList(Vec<PeerInfo>),
}
