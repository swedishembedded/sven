use sven_config::AgentMode;
use sven_tools::{events::TodoItem, ToolCall};

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
    /// The todo list was updated
    TodoUpdate(Vec<TodoItem>),
    /// The agent mode was changed
    ModeChanged(AgentMode),
    /// The agent is asking the user a question (id links to QuestionAnswer)
    Question { id: String, questions: Vec<String> },
    /// Answer to a previous Question event
    QuestionAnswer { id: String, answer: String },
}
