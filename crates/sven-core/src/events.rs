use sven_tools::ToolCall;

/// Events emitted by the agent during a single turn.
/// Consumers (CI runner, TUI) subscribe to these to drive their output.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A text chunk streamed from the model
    TextDelta(String),
    /// A complete text response from the model (after streaming finishes)
    TextComplete(String),
    /// The model has requested a tool call
    ToolCallStarted(ToolCall),
    /// A tool call finished
    ToolCallFinished {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    /// Context was compacted; statistics for the UI
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
    },
    /// Current token usage update
    TokenUsage { input: u32, output: u32, context_total: usize },
    /// The agent has finished processing the current user turn
    TurnComplete,
    /// A recoverable error occurred
    Error(String),
}
