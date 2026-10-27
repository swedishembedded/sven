// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tool calls a session asks permission for, asked of the server.
//!
//! A tool whose policy is to ask before it runs would otherwise run unasked
//! on a host nobody is watching: the person driving the session is at the
//! server's interface, not this terminal. Each such call is sent up as
//! `ToolNeedsApproval` and waits for the server's `ApproveTool` or
//! `DenyTool`. A call nobody answers within [`APPROVAL_TIMEOUT`] is denied:
//! silence is never consent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_control::ControlEvent;
use sven_hsm::ToolCapability;
use sven_tool_api::{PermissionRequester, ToolCall};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// How long a call waits for an answer before it is denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// The calls of one session waiting for an answer, by call id.
#[derive(Clone, Default)]
pub struct PendingApprovals(Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>);

impl PendingApprovals {
    /// Answers call `call_id`. False when nothing was waiting for it (already
    /// answered, timed out, or never asked).
    pub fn decide(&self, call_id: &str, approved: bool) -> bool {
        let waiting = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(call_id);
        waiting.is_some_and(|answer| answer.send(approved).is_ok())
    }

    fn wait(&self, call_id: &str) -> oneshot::Receiver<bool> {
        let (answer, waiting) = oneshot::channel();
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(call_id.to_string(), answer);
        waiting
    }

    fn forget(&self, call_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(call_id);
    }
}

/// Asks the server about one session's calls.
pub struct RemoteApprovals {
    pub(crate) session_id: Uuid,
    pub(crate) pending: PendingApprovals,
    /// Where the request goes: the host's uploader.
    pub(crate) ask: mpsc::Sender<ControlEvent>,
    pub(crate) timeout: Duration,
}

#[async_trait::async_trait]
impl PermissionRequester for RemoteApprovals {
    async fn request_permission(&self, call: &ToolCall, _capability: ToolCapability) -> bool {
        let answer = self.pending.wait(&call.id);
        let request = ControlEvent::ToolNeedsApproval {
            session_id: self.session_id,
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            args: call.args.clone(),
        };
        println!(
            "[{}]   {} waits for approval in the conversation",
            &self.session_id.to_string()[..8],
            call.name
        );
        if self.ask.send(request).await.is_err() {
            self.pending.forget(&call.id);
            return false;
        }
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(approved)) => approved,
            _ => {
                self.pending.forget(&call.id);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requester(timeout: Duration) -> (RemoteApprovals, mpsc::Receiver<ControlEvent>) {
        let (ask, asked) = mpsc::channel(4);
        let approvals = RemoteApprovals {
            session_id: Uuid::new_v4(),
            pending: PendingApprovals::default(),
            ask,
            timeout,
        };
        (approvals, asked)
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "shell".into(),
            args: serde_json::json!({"command": "rm -rf build"}),
        }
    }

    #[tokio::test]
    async fn a_call_runs_only_when_the_server_approves_it() {
        for approve in [true, false] {
            let (approvals, mut asked) = requester(Duration::from_secs(5));
            let pending = approvals.pending.clone();
            let decision = tokio::spawn(async move {
                approvals
                    .request_permission(&call("c1"), ToolCapability::ExecuteShell)
                    .await
            });
            match asked.recv().await.expect("asked") {
                ControlEvent::ToolNeedsApproval {
                    call_id, tool_name, ..
                } => {
                    assert_eq!((call_id.as_str(), tool_name.as_str()), ("c1", "shell"));
                }
                other => panic!("expected ToolNeedsApproval, got {other:?}"),
            }
            assert!(pending.decide("c1", approve));
            assert_eq!(decision.await.unwrap(), approve);
            assert!(
                !pending.decide("c1", true),
                "an answered call takes no second answer"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_nobody_answers_is_denied() {
        let (approvals, mut asked) = requester(APPROVAL_TIMEOUT);
        let pending = approvals.pending.clone();
        let decision = tokio::spawn(async move {
            approvals
                .request_permission(&call("c2"), ToolCapability::ExecuteShell)
                .await
        });
        asked.recv().await.expect("asked");
        assert!(!decision.await.unwrap(), "silence is not consent");
        assert!(
            !pending.decide("c2", true),
            "a late answer finds nothing waiting"
        );
    }
}
