// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A sub-agent's calls, stoppable one by one from the parent.
//!
//! The person can stop one call of the parent's turn by its id while it
//! runs. The calls a sub-agent runs for that turn are shown to them the same
//! way, by the ids the child reports them under, so each is registered with
//! the parent's executor ([`sven_tool_api::nested_calls`]) for as long as the
//! child says it runs. A stop of one is passed to the child as
//! [`CANCEL_TOOL_METHOD`], which stops that call alone; the child's turn,
//! and the parent's, go on.
//!
//! Swedish Embedded AB implements solutions for agents that delegate work
//! and stay in the person's control for its clients. If your team needs
//! expertise in multi-agent orchestration then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::{SessionUpdate, ToolCallStatus};
use sven_tool_api::nested_calls::NestedCalls;
use tokio::sync::{mpsc, oneshot};

use super::{CancelToolParams, CANCEL_TOOL_METHOD};

/// The child's calls running now, registered with the parent's executor.
pub(super) struct ChildCallStops {
    calls: Option<Arc<dyn NestedCalls>>,
    /// Each running call's end: dropping it ends the call's registration.
    running: HashMap<String, oneshot::Sender<()>>,
    stopped_tx: mpsc::UnboundedSender<String>,
}

impl ChildCallStops {
    /// Registers with `calls` (none: the parent's executor offers no
    /// stopping of calls, and nothing is registered). The ids of the calls
    /// the person stops come out of the returned receiver.
    pub(super) fn new(
        calls: Option<Arc<dyn NestedCalls>>,
    ) -> (Self, mpsc::UnboundedReceiver<String>) {
        let (stopped_tx, stopped_rx) = mpsc::unbounded_channel();
        let stops = Self {
            calls,
            running: HashMap::new(),
            stopped_tx,
        };
        (stops, stopped_rx)
    }

    /// Follows what the child said about its calls: one that started is
    /// registered, one that ended is let go. Runs inside a `LocalSet`.
    pub(super) fn note(&mut self, update: &SessionUpdate) {
        let Some(calls) = &self.calls else {
            return;
        };
        let SessionUpdate::ToolCall(call) = update else {
            return;
        };
        let id = call.tool_call_id.0.to_string();
        match call.status {
            ToolCallStatus::Pending | ToolCallStatus::InProgress => {
                if self.running.contains_key(&id) {
                    return;
                }
                let (ended_tx, ended_rx) = oneshot::channel::<()>();
                self.running.insert(id.clone(), ended_tx);
                let mut registered = calls.register(&id);
                let stopped_tx = self.stopped_tx.clone();
                tokio::task::spawn_local(async move {
                    tokio::select! {
                        () = registered.stopped() => {
                            let _ = stopped_tx.send(id);
                        }
                        _ = ended_rx => {}
                    }
                });
            }
            _ => {
                self.running.remove(&id);
            }
        }
    }
}

/// The extension notification that stops the child's call `tool_call_id` in
/// its session `session_id`.
pub(super) fn cancel_tool_notification(
    session_id: &str,
    tool_call_id: String,
) -> agent_client_protocol::ExtNotification {
    let params = CancelToolParams {
        session_id: session_id.to_string(),
        tool_call_id,
    };
    let raw = serde_json::value::to_raw_value(&params).expect("two strings serialise");
    agent_client_protocol::ExtNotification::new(CANCEL_TOOL_METHOD, raw.into())
}
