//! A `tracing_subscriber::Layer` that captures log records from the P2P
//! subsystem and forwards them to a `broadcast::Sender<LogEntry>`.
//!
//! This decouples the P2P crate from whatever logging/TUI setup the host
//! application uses — the host subscribes to the channel and displays the
//! log entries however it likes, without them going to stdout/stderr.

use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::protocol::types::LogEntry;

/// Capacity of the log broadcast channel (number of buffered entries per subscriber).
pub const LOG_CHANNEL_CAPACITY: usize = 512;

/// Creates a paired `(layer, receiver)`.
///
/// Install `layer` in a `tracing_subscriber::Registry` alongside any other
/// layers used by the host application. Subscribe to `receiver` (or call
/// `handle.subscribe_logs()`) to receive buffered log entries.
pub fn build_log_channel() -> (LogCaptureLayer, broadcast::Receiver<LogEntry>) {
    let (tx, rx) = broadcast::channel(LOG_CHANNEL_CAPACITY);
    (LogCaptureLayer { tx }, rx)
}

/// A tracing layer that converts each log `Event` into a `LogEntry` and sends
/// it over a broadcast channel.
///
/// Dropped senders (lagged receivers) are silently ignored — the P2P loop
/// never blocks on the channel.
pub struct LogCaptureLayer {
    tx: broadcast::Sender<LogEntry>,
}

impl LogCaptureLayer {
    /// Subscribe an additional receiver to this layer's output.
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }
}

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let entry = LogEntry {
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            message: visitor.0,
        };
        // Ignore send errors (no subscribers or channel full).
        let _ = self.tx.send(entry);
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        } else if !self.0.is_empty() {
            self.0.push_str(&format!(", {}={:?}", field.name(), value));
        } else {
            self.0 = format!("{}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        } else if !self.0.is_empty() {
            self.0.push_str(&format!(", {}={}", field.name(), value));
        } else {
            self.0 = format!("{}={}", field.name(), value);
        }
    }
}
