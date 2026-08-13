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

use tokio::sync::broadcast;

/// A renderable event emitted on the outward observation plane.
///
/// Re-exports [`sven_vocab::SessionEvent`] — the single, unified session
/// event stream (`sven-core` re-exports the same type as `AgentEvent`). They
/// never re-enter the inward event queue; they exist purely so frontends
/// (TUI, GUI, CI, node, ACP) can render streaming output, tool progress,
/// usage, and the transition trace while effects are in flight.
pub use sven_vocab::SessionEvent as UiEvent;
/// Re-export of [`UiEvent::ContextCompacted`]'s `strategy` field type.
pub use sven_vocab::CompactionStrategyUsed;

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
