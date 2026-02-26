// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use sven_config::AgentMode;
use sven_tools::{events::TodoItem, ToolCall};

/// Which compaction strategy was executed when `ContextCompacted` fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionStrategyUsed {
    /// Structured Markdown checkpoint with typed sections.
    Structured,
    /// Legacy free-form narrative summary.
    Narrative,
    /// Emergency fallback: history was dropped without a model summary call
    /// because the session was too large to fit even a compaction prompt.
    Emergency,
}

impl std::fmt::Display for CompactionStrategyUsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionStrategyUsed::Structured => write!(f, "structured"),
            CompactionStrategyUsed::Narrative => write!(f, "narrative"),
            CompactionStrategyUsed::Emergency => write!(f, "emergency"),
        }
    }
}

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
    /// The todo list was updated
    TodoUpdate(Vec<TodoItem>),
    /// The agent mode was changed
    ModeChanged(AgentMode),
    /// The agent is asking the user a question (id links to QuestionAnswer)
    Question { id: String, questions: Vec<String> },
    /// Answer to a previous Question event
    QuestionAnswer { id: String, answer: String },
}
