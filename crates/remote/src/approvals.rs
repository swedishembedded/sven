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
//!
//! Each request names the turn it belongs to ([`TurnAuthority`]): the answer
//! the server showed for the input that started it, so the server judges it
//! by that turn, never by whichever it is answering when the request
//! arrives. A turn someone other than the owner started asks about every
//! call, a tool that would run unasked included.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_control::ControlEvent;
use sven_hsm::ToolCapability;
use sven_tool_api::{Permission, PermissionRequester, ToolCall};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// How long a call waits for an answer before it is denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// The calls of one session waiting for an answer, by call id.
#[derive(Clone, Default)]
pub struct PendingApprovals(Arc<Mutex<HashMap<String, oneshot::Sender<Permission>>>>);

impl PendingApprovals {
    /// Answers call `call_id`; a denial's `reason` is passed on to the agent.
    /// False when nothing was waiting for it (already answered, timed out, or
    /// never asked).
    pub fn decide(&self, call_id: &str, approved: bool, reason: Option<String>) -> bool {
        let waiting = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(call_id);
        let answer = if approved {
            Permission::Allowed
        } else {
            Permission::Denied { reason }
        };
        waiting.is_some_and(|waiting| waiting.send(answer).is_ok())
    }

    fn wait(&self, call_id: &str) -> oneshot::Receiver<Permission> {
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

/// The turn a session runs: the answer it writes, and whether it puts every
/// tool call to the server. Set by the session as each turn starts, read by
/// its requester as each call asks.
#[derive(Clone, Default)]
pub struct TurnAuthority(Arc<Mutex<Turn>>);

/// What [`TurnAuthority`] holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    pub answer_id: Option<String>,
    pub ask_every_tool: bool,
}

impl TurnAuthority {
    pub fn set(&self, turn: Turn) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = turn;
    }

    pub fn get(&self) -> Turn {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Asks the server about one session's calls.
pub struct RemoteApprovals {
    pub(crate) session_id: Uuid,
    pub(crate) pending: PendingApprovals,
    /// Where the request goes: the host's uploader.
    pub(crate) ask: mpsc::Sender<ControlEvent>,
    pub(crate) timeout: Duration,
    /// The turn the calls belong to.
    pub(crate) turn: TurnAuthority,
    /// Calls the server already approved: one asked of it twice (by the
    /// session's approval mode, then by the tool registry under
    /// [`Turn::ask_every_tool`]) is not asked again.
    pub(crate) approved: Arc<Mutex<HashSet<String>>>,
}

#[async_trait::async_trait]
impl PermissionRequester for RemoteApprovals {
    async fn request_permission(&self, call: &ToolCall, capability: ToolCapability) -> bool {
        self.decide(call, capability).await == Permission::Allowed
    }

    async fn decide(&self, call: &ToolCall, _capability: ToolCapability) -> Permission {
        if self
            .approved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&call.id)
        {
            return Permission::Allowed;
        }
        let answer = self.pending.wait(&call.id);
        let request = ControlEvent::ToolNeedsApproval {
            session_id: self.session_id,
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            args: call.args.clone(),
            answer_id: self.turn.get().answer_id,
        };
        println!(
            "[{}]   {} waits for approval in the conversation",
            &self.session_id.to_string()[..8],
            crate::printable::printable(&call.name)
        );
        if self.ask.send(request).await.is_err() {
            self.pending.forget(&call.id);
            return Permission::Denied { reason: None };
        }
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(answer)) => {
                if answer == Permission::Allowed {
                    self.approved
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(call.id.clone());
                }
                answer
            }
            _ => {
                self.pending.forget(&call.id);
                Permission::Denied { reason: None }
            }
        }
    }

    fn asks_every_call(&self) -> bool {
        self.turn.get().ask_every_tool
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
            turn: TurnAuthority::default(),
            approved: Arc::default(),
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
            assert!(pending.decide("c1", approve, None));
            assert_eq!(decision.await.unwrap(), approve);
            assert!(
                !pending.decide("c1", true, None),
                "an answered call takes no second answer"
            );
        }
    }

    /// Spec: a denial's reason from the server reaches the agent's tool call.
    #[tokio::test]
    async fn a_denial_carries_its_reason() {
        let (approvals, mut asked) = requester(Duration::from_secs(5));
        let pending = approvals.pending.clone();
        let decision = tokio::spawn(async move {
            approvals
                .decide(&call("c4"), ToolCapability::WriteFile)
                .await
        });
        asked.recv().await.expect("asked");
        assert!(pending.decide("c4", false, Some("use a model that keeps no data".into())));
        assert_eq!(
            decision.await.unwrap(),
            Permission::Denied {
                reason: Some("use a model that keeps no data".into())
            }
        );
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
            !pending.decide("c2", true, None),
            "a late answer finds nothing waiting"
        );
    }

    /// Spec: a request names the answer of the turn it belongs to, a turn
    /// someone other than the owner started asks about every call, and a
    /// call the server approved is not asked about again.
    #[tokio::test]
    async fn a_request_names_its_turn_and_a_shared_turn_asks_every_call() {
        let (approvals, mut asked) = requester(Duration::from_secs(5));
        assert!(
            !approvals.asks_every_call(),
            "the owner's turn asks as its tools say"
        );
        approvals.turn.set(Turn {
            answer_id: Some("a1".into()),
            ask_every_tool: true,
        });
        assert!(approvals.asks_every_call());
        let pending = approvals.pending.clone();
        let approvals = Arc::new(approvals);
        let asking = approvals.clone();
        let decision = tokio::spawn(async move {
            asking
                .request_permission(&call("c3"), ToolCapability::ReadFile)
                .await
        });
        match asked.recv().await.expect("asked") {
            ControlEvent::ToolNeedsApproval { answer_id, .. } => {
                assert_eq!(answer_id.as_deref(), Some("a1"));
            }
            other => panic!("expected ToolNeedsApproval, got {other:?}"),
        }
        assert!(pending.decide("c3", true, None));
        assert!(decision.await.unwrap());
        // Asked again (the registry after the approval mode): not put twice.
        assert!(
            approvals
                .request_permission(&call("c3"), ToolCapability::ReadFile)
                .await
        );
        assert!(
            asked.try_recv().is_err(),
            "an approved call is not asked twice"
        );
    }
}
