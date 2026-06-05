//! The outward observation plane.
//!
//! Two strictly separated data planes drive a session:
//!
//! * **Inward plane (RTC):** the per-session [`Event`](crate::event::Event)
//!   queue. One event is fully dispatched at a time; pure transitions return
//!   `Vec<Effect>`. This is what guarantees run-to-completion.
//! * **Outward plane (this module):** a per-session broadcast
//!   [`ObservationSink`] of [`UiEvent`]s. Executors emit streaming
//!   deltas / tool-progress / usage outward *while an effect is in flight*,
//!   then post exactly **one** completion [`Event`](crate::event::Event) back
//!   inward. RTC is preserved because streaming never re-enters the queue.
//!
//! The kernel also emits a [`UiEvent::Transition`] on this bus after every
//! dispatch, giving observers a full transition trace.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

/// A renderable event emitted on the outward observation plane.
///
/// `UiEvent`s mirror the renderable subset of the legacy agent event stream.
/// They never re-enter the inward event queue; they exist purely so frontends
/// (TUI, GUI, CI, node, ACP) can render streaming output, tool progress,
/// usage, and the transition trace while effects are in flight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum UiEvent {
    /// A streamed chunk of assistant text.
    TextDelta(String),
    /// The complete assistant text for a turn (after streaming finishes).
    TextComplete(String),
    /// A streamed chunk of model "thinking" / reasoning.
    ThinkingDelta(String),
    /// The complete thinking block (accumulated from `ThinkingDelta`).
    ThinkingComplete(String),
    /// A tool invocation has begun.
    ToolStarted {
        /// Correlates with the originating tool call.
        call_id: String,
        /// Tool name.
        name: String,
        /// Tool arguments.
        args: Value,
    },
    /// A long-running tool is reporting incremental progress.
    ToolProgress {
        /// Correlates with the originating tool call.
        call_id: String,
        /// Short human-readable status line.
        message: String,
    },
    /// A tool invocation has finished.
    ToolFinished {
        /// Correlates with the originating tool call.
        call_id: String,
        /// Tool name.
        name: String,
        /// Tool output (truncated for display by the frontend if needed).
        output: String,
        /// `true` if the tool reported an error.
        is_error: bool,
    },
    /// Token usage update for the current turn.
    TokenUsage {
        /// Input tokens processed this request (excludes cache hits).
        input: u32,
        /// Output tokens generated this request.
        output: u32,
        /// Tokens served from the provider's prompt cache this turn.
        cache_read: u32,
        /// Tokens written to the provider's prompt cache this turn.
        cache_write: u32,
        /// Running total of cache-read tokens for the session.
        cache_read_total: u32,
        /// Running total of cache-write tokens for the session.
        cache_write_total: u32,
        /// Model context window (tokens); zero if unknown.
        max_tokens: usize,
        /// Model max output tokens; zero if unknown.
        max_output_tokens: usize,
        /// Cost in USD when reported by the provider.
        cost_usd: Option<f64>,
    },
    /// The session context was compacted.
    ContextCompacted {
        /// Token count before compaction.
        tokens_before: usize,
        /// Token count after compaction.
        tokens_after: usize,
        /// Strategy used (e.g. `"structured"`, `"narrative"`, `"emergency"`).
        strategy: String,
        /// Agentic round in which compaction fired (0 = pre-submit).
        turn: u32,
    },
    /// The todo list was updated (opaque structured payload).
    TodoUpdate(Value),
    /// The agent mode changed.
    ModeChanged(String),
    /// The active model changed (resolved `provider/id`).
    ModelChanged(String),
    /// The machine took a transition (full transition trace).
    Transition {
        /// State label before the dispatch.
        from: String,
        /// State label after the dispatch.
        to: String,
        /// The kind of event dispatched.
        event: String,
    },
    /// A recoverable error occurred.
    Error(String),
    /// The current user turn finished.
    TurnComplete,
    /// The current run was aborted; carries any streamed-but-uncommitted text.
    Aborted {
        /// Partial assistant text streamed before the abort.
        partial_text: String,
    },
}

/// A broadcast sender for [`UiEvent`]s — the outward observation plane.
///
/// Cloneable and cheap. Sends are lossy when a subscriber lags beyond the
/// channel capacity (the slow subscriber observes a `Lagged` error); this is
/// intentional, since the inward plane remains the source of truth.
#[derive(Clone)]
pub struct ObservationSink {
    tx: broadcast::Sender<UiEvent>,
}

impl ObservationSink {
    /// Creates a sink with the given channel capacity (minimum 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Emits a [`UiEvent`]. Returns the number of receivers that observed it
    /// (zero when nobody is subscribed; never an error).
    pub fn emit(&self, event: UiEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribes a new receiver to the observation stream.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<UiEvent> {
        self.tx.subscribe()
    }

    /// The number of active subscribers.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for ObservationSink {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_reaches_subscriber() {
        let sink = ObservationSink::new(8);
        let mut rx = sink.subscribe();
        sink.emit(UiEvent::TextDelta("hi".into()));
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev, UiEvent::TextDelta("hi".into()));
    }

    #[test]
    fn emit_without_subscriber_is_ok() {
        let sink = ObservationSink::new(8);
        assert_eq!(sink.emit(UiEvent::TurnComplete), 0);
    }
}
