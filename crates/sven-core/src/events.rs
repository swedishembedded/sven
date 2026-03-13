// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use sven_config::AgentMode;
use sven_tools::{
    events::{SubagentUpdate, TodoItem},
    ToolCall,
};

/// Information about a connected peer (node proxy / list_peers).
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    pub peer_id: String,
    pub connected: bool,
    pub can_delegate: bool,
}

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
        /// The model's maximum output tokens per completion.  Zero means unknown.
        /// The usable input budget is `max_tokens − max_output_tokens`.
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
    /// A completed delegate subtree — rendered as a collapsible `DelegateSummary` segment.
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

/// Visitor trait for [`AgentEvent`].
///
/// Consumers (CI runner, TUI, ACP bridge) that need to react to agent events
/// implement this trait and override only the variants they care about.
/// Every method has a default no-op implementation so new event variants can
/// be added to [`AgentEvent`] without breaking all existing consumers at once.
///
/// The trait is deliberately **synchronous** — async consumers (e.g. the TUI)
/// use it as a documentation contract and handle events in their own async
/// match blocks.
pub trait AgentEventVisitor {
    fn on_text_delta(&mut self, _delta: &str) {}
    fn on_text_complete(&mut self, _text: &str) {}
    fn on_thinking_delta(&mut self, _delta: &str) {}
    fn on_thinking_complete(&mut self, _content: &str) {}
    fn on_tool_call_started(&mut self, _call: &sven_tools::ToolCall) {}
    fn on_tool_call_finished(
        &mut self,
        _call_id: &str,
        _tool_name: &str,
        _output: &str,
        _is_error: bool,
    ) {
    }
    fn on_context_compacted(
        &mut self,
        _tokens_before: usize,
        _tokens_after: usize,
        _strategy: &CompactionStrategyUsed,
        _turn: u32,
    ) {
    }
    #[allow(clippy::too_many_arguments)]
    fn on_token_usage(
        &mut self,
        _input: u32,
        _output: u32,
        _cache_read: u32,
        _cache_write: u32,
        _cache_read_total: u32,
        _cache_write_total: u32,
        _max_tokens: usize,
        _max_output_tokens: usize,
    ) {
    }
    fn on_turn_complete(&mut self) {}
    fn on_aborted(&mut self, _partial_text: &str) {}
    fn on_error(&mut self, _message: &str) {}
    fn on_tool_progress(&mut self, _call_id: &str, _message: &str) {}
    fn on_todo_update(&mut self, _todos: &[sven_tools::events::TodoItem]) {}
    fn on_mode_changed(&mut self, _mode: &sven_config::AgentMode) {}
    fn on_model_changed(&mut self, _model: &str) {}
    fn on_question(&mut self, _id: &str, _questions: &[String]) {}
    fn on_question_answer(&mut self, _id: &str, _answer: &str) {}
    fn on_title_generated(&mut self, _title: &str) {}
    fn on_collab_event(&mut self, _event: &crate::prompts::CollabEvent) {}
    fn on_delegate_summary(
        &mut self,
        _to_name: &str,
        _task_title: &str,
        _duration_ms: u64,
        _status: &str,
        _result_preview: &str,
    ) {
    }
    fn on_subagent_started(
        &mut self,
        _call_id: &str,
        _handle_id: &str,
        _description: &str,
        _prompt: &str,
    ) {
    }
    fn on_subagent_event(
        &mut self,
        _call_id: &str,
        _handle_id: &str,
        _update: &sven_tools::events::SubagentUpdate,
    ) {
    }
    fn on_peer_list(&mut self, _peers: &[PeerInfo]) {}

    /// Dispatch an [`AgentEvent`] to the appropriate visitor method.
    fn visit(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta(d) => self.on_text_delta(d),
            AgentEvent::TextComplete(t) => self.on_text_complete(t),
            AgentEvent::ThinkingDelta(d) => self.on_thinking_delta(d),
            AgentEvent::ThinkingComplete(c) => self.on_thinking_complete(c),
            AgentEvent::ToolCallStarted(tc) => self.on_tool_call_started(tc),
            AgentEvent::ToolCallFinished {
                call_id,
                tool_name,
                output,
                is_error,
            } => self.on_tool_call_finished(call_id, tool_name, output, *is_error),
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            } => self.on_context_compacted(*tokens_before, *tokens_after, strategy, *turn),
            AgentEvent::TokenUsage {
                input,
                output,
                cache_read,
                cache_write,
                cache_read_total,
                cache_write_total,
                max_tokens,
                max_output_tokens,
                cost_usd: _,
            } => self.on_token_usage(
                *input,
                *output,
                *cache_read,
                *cache_write,
                *cache_read_total,
                *cache_write_total,
                *max_tokens,
                *max_output_tokens,
            ),
            AgentEvent::TurnComplete => self.on_turn_complete(),
            AgentEvent::Aborted { partial_text } => self.on_aborted(partial_text),
            AgentEvent::Error(m) => self.on_error(m),
            AgentEvent::ToolProgress { call_id, message } => {
                self.on_tool_progress(call_id, message)
            }
            AgentEvent::TodoUpdate(todos) => self.on_todo_update(todos),
            AgentEvent::ModeChanged(mode) => self.on_mode_changed(mode),
            AgentEvent::ModelChanged(model) => self.on_model_changed(model),
            AgentEvent::Question { id, questions } => self.on_question(id, questions),
            AgentEvent::QuestionAnswer { id, answer } => self.on_question_answer(id, answer),
            AgentEvent::TitleGenerated(t) => self.on_title_generated(t),
            AgentEvent::CollabEvent(e) => self.on_collab_event(e),
            AgentEvent::DelegateSummary {
                to_name,
                task_title,
                duration_ms,
                status,
                result_preview,
            } => {
                self.on_delegate_summary(to_name, task_title, *duration_ms, status, result_preview)
            }
            AgentEvent::SubagentStarted {
                call_id,
                handle_id,
                description,
                prompt,
            } => self.on_subagent_started(call_id, handle_id, description, prompt),
            AgentEvent::SubagentEvent {
                call_id,
                handle_id,
                update,
            } => self.on_subagent_event(call_id, handle_id, update),
            AgentEvent::PeerList(peers) => self.on_peer_list(peers),
        }
    }
}
