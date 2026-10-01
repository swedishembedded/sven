// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a caller gets back when a kernel session is built.
//!
//! [`KernelChannels`] is the inward half - the questions and approval gates a
//! session needs answered - and [`RuntimeHandle`] is the outward half: posting
//! events, subscribing to observations, and reading status. Kept together
//! because they are two ends of the same contract, and apart from the builder
//! because neither is involved in assembling anything.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use sven_executors::ThreadStore;
use sven_executors::{ApprovalRequest, UserQuestion};
use sven_hsm::ToolCapability;
use sven_hsm::{Event, RuntimeStatus};
use sven_kernel::EventSink;
use sven_model::Message;
use sven_tool_registry::ToolRegistry;

// ── KernelChannels ────────────────────────────────────────────────────────────

/// Channel endpoints returned to the caller (TUI / node / CI) so they can
/// exchange user input and approval decisions with the running kernel.
pub struct KernelChannels {
    /// Receives questions the kernel's `UserExecutor` forwards from
    /// `Effect::AskUser`. The holder must display the prompt and send the
    /// answer through [`UserQuestion::reply_tx`].
    pub question_rx: mpsc::Receiver<UserQuestion>,
    /// Receives approval requests forwarded from
    /// `Effect::RequestHumanApproval`. The holder must approve or deny via
    /// [`ApprovalRequest::reply_tx`].
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
}

/// Puts a tool call to a session's own approval gate: the channel its
/// `UserExecutor` sends `Effect::RequestHumanApproval` down, so the person
/// sees the request exactly as they see the session's own approvals.
///
/// Used where something outside the kernel needs that person's consent - a
/// `task` sub-agent's permission request the session's policy does not allow
/// outright. A gate nobody holds any more (the channel closed) refuses.
pub struct GateApprover {
    approvals: mpsc::Sender<ApprovalRequest>,
    preapproved: Option<sven_executors::Preapproval>,
}

impl GateApprover {
    /// An approver that asks through `approvals`.
    #[must_use]
    pub fn new(approvals: mpsc::Sender<ApprovalRequest>) -> Self {
        Self {
            approvals,
            preapproved: None,
        }
    }

    /// Approves a call `preapproved` accepts without asking, as the
    /// session's own `UserExecutor` does.
    #[must_use]
    pub fn with_preapproval(mut self, preapproved: sven_executors::Preapproval) -> Self {
        self.preapproved = Some(preapproved);
        self
    }
}

#[async_trait::async_trait]
impl sven_tool_api::PermissionRequester for GateApprover {
    async fn request_permission(
        &self,
        call: &sven_tool_api::ToolCall,
        capability: sven_hsm::ToolCapability,
    ) -> bool {
        let gated = sven_hsm::GatedCall {
            name: call.name.clone(),
            args: call.args.clone(),
        };
        if self.preapproved.as_ref().is_some_and(|p| p(&gated)) {
            return true;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = ApprovalRequest {
            approval_id: sven_hsm::ApprovalId::new(),
            capability,
            description: format!("a sub-agent wants to run the tool `{}`", call.name),
            call: Some(gated),
            reply_tx,
        };
        if self.approvals.send(request).await.is_err() {
            return false;
        }
        reply_rx.await.unwrap_or(false)
    }
}

impl KernelChannels {
    /// Channels nobody sends on: what a bundle keeps once its live channels
    /// were handed to whoever answers them.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            question_rx: mpsc::channel(1).1,
            approval_rx: mpsc::channel(1).1,
        }
    }

    /// Hands every kernel-level question and approval gate to `responder`,
    /// which owns replying to each.
    ///
    /// The alternative to [`Self::answer_unattended`] for a host that has a
    /// person (or its own policy) to answer. Returns once both channels
    /// close.
    pub async fn forward_to(mut self, responder: HumanGateResponder) {
        loop {
            tokio::select! {
                q = self.question_rx.recv() => match q {
                    Some(q) => responder(HumanGate::Question {
                        prompt: q.prompt,
                        reply_tx: q.reply_tx,
                    }),
                    None => break,
                },
                a = self.approval_rx.recv() => match a {
                    Some(a) => responder(HumanGate::Approval {
                        capability: a.capability,
                        prompt: a.description,
                        call: a.call,
                        reply_tx: a.reply_tx,
                    }),
                    None => break,
                },
            }
        }
    }

    /// Answers every kernel-level question and approval gate at once, so a
    /// session nobody is at never blocks: every `AskUser` gets
    /// [`NO_USER_ANSWER`](sven_tool_api::NO_USER_ANSWER), and every approval
    /// request is refused. Returns once both channels close.
    ///
    /// Only a session under manual approval puts anything on the approval
    /// channel (under auto the `UserExecutor` approves decisions itself and
    /// no tool call asks), and manual approval is never started without a
    /// person to answer - so a request here has nobody to approve it, and is
    /// not approved on their behalf.
    ///
    /// This is the unattended path - headless and CI runs, `acp serve`,
    /// dispatched steps. Typically driven with
    /// `tokio::spawn(channels.answer_unattended())`.
    ///
    /// **Prefer [`Self::forward_to`] whenever the host CAN answer.**
    pub async fn answer_unattended(mut self) {
        loop {
            tokio::select! {
                q = self.question_rx.recv() => match q {
                    Some(q) => { let _ = q.reply_tx.send(sven_tool_api::NO_USER_ANSWER.to_string()); }
                    None => break,
                },
                a = self.approval_rx.recv() => match a {
                    Some(a) => { let _ = a.reply_tx.send(false); }
                    None => break,
                },
            }
        }
    }
}

/// Records every question a session parks in the question ledger, where
/// `sven questions` finds it, and returns the channel the session's
/// `UserExecutor` sends them down.
#[cfg(feature = "memory")]
pub(crate) fn spawn_parked_question_ledger() -> mpsc::Sender<sven_executors::user::ParkedQuestion> {
    let (parked_tx, mut parked_rx) = sven_executors::UserExecutor::parked_channel(16);
    tokio::spawn(async move {
        let ledger = sven_memory::QuestionLedger::at_default_path();
        while let Some(q) = parked_rx.recv().await {
            let asked_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Err(err) = ledger.record_asked(&sven_memory::QuestionAskedRecord {
                question_id: q.question_id,
                call_id: q.call_id,
                prompt: q.prompt,
                options: q.options,
                asked_at,
            }) {
                tracing::warn!(
                    error = %err,
                    "failed to record a parked question; it will not appear in `sven questions list`"
                );
            }
        }
    });
    parked_tx
}

// ── RuntimeHandle ─────────────────────────────────────────────────────────────

/// One kernel-level gate, handed to a host that wants to answer it itself.
///
/// Carries its own reply channel, so the responder OWNS answering: it may
/// reply now, or hold the channel while it asks someone and reply minutes
/// later. That is the whole point - a host that could only answer
/// synchronously could not ask a person, which is the one thing a human gate
/// is for.
///
/// Dropping a gate without replying is not a silent default: the kernel's
/// turn stays parked, exactly as it would if a person never answered.
pub enum HumanGate {
    /// `Effect::AskUser` - free text.
    Question {
        /// What to show the person.
        prompt: String,
        /// Their answer.
        reply_tx: oneshot::Sender<String>,
    },
    /// `Effect::RequestHumanApproval` - yes or no.
    Approval {
        /// What capability is being asked for.
        capability: ToolCapability,
        /// What will happen if this is approved.
        prompt: String,
        /// The tool call it gates, with its arguments, when it gates one.
        call: Option<sven_hsm::GatedCall>,
        /// `true` to approve.
        reply_tx: oneshot::Sender<bool>,
    },
}

/// Where a host receives the gates it has chosen to answer itself.
///
/// A plain callback rather than yet another channel: the host already has
/// somewhere to put these (its own run state, its own UI), and handing it the
/// gate directly means it never has to keep a task alive just to move values
/// from one queue to another.
pub type HumanGateResponder = Arc<dyn Fn(HumanGate) + Send + Sync>;

/// A cheap-to-clone handle to a spawned [`sven_kernel::ErasedRuntime`].
///
/// Provides the event sink and status watch; the caller typically also holds
/// the [`KernelChannels`] returned alongside this handle.
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(crate) sink: EventSink,
    pub(crate) obs: sven_hsm::ObservationSink,
    pub(crate) status_rx: watch::Receiver<RuntimeStatus>,
    /// The kernel's shared conversation store (thread → turns). Exposed so
    /// interactive frontends can seed / replace history mid-session for the
    /// edit-resubmit and resume flows.
    pub(crate) conv_store: Arc<std::sync::Mutex<ThreadStore>>,
    /// The live tool registry. Exposed so frontends can hot-swap MCP tools via
    /// [`ToolRegistry::replace_mcp_tools`] without rebuilding the session.
    pub(crate) tool_registry: Arc<ToolRegistry>,
}

impl RuntimeHandle {
    /// Returns a cloneable sink for posting events into the kernel.
    #[must_use]
    pub fn sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// Returns a clone of the outward observation sink for this session.
    #[must_use]
    pub fn observations(&self) -> sven_hsm::ObservationSink {
        self.obs.clone()
    }

    /// Subscribes a fresh receiver to the outward observation plane
    /// (`UiEvent` stream: streamed text, tool progress, usage, transitions).
    #[must_use]
    pub fn subscribe_observations(&self) -> tokio::sync::broadcast::Receiver<sven_hsm::UiEvent> {
        self.obs.subscribe()
    }

    /// Posts `Event::UserMessage { text }` into the kernel queue.
    pub async fn send_user_message(&self, text: String) -> bool {
        self.sink.emit(Event::UserMessage { text }).await
    }

    /// Posts `Event::UserCancelled` into the kernel queue.
    pub async fn cancel(&self) -> bool {
        self.sink.emit(Event::UserCancelled).await
    }

    /// The latest published status snapshot.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.status_rx.borrow().clone()
    }

    /// A fresh receiver for status updates (watch channel).
    #[must_use]
    pub fn status_watch(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_rx.clone()
    }

    /// The kernel's shared conversation store (for history seeding / resume).
    #[must_use]
    pub fn conversation_store(&self) -> Arc<std::sync::Mutex<ThreadStore>> {
        Arc::clone(&self.conv_store)
    }

    /// A snapshot of the reactive-agent conversation thread.
    ///
    /// Used to carry accumulated context forward when a session is rebuilt
    /// (e.g. on a mid-session model switch) so the replacement kernel can be
    /// seeded with the same history. Empty if the store mutex is poisoned.
    #[must_use]
    pub fn history_snapshot(&self) -> Vec<Message> {
        self.conv_store
            .lock()
            .ok()
            .map(|store| store.snapshot(sven_machines::machines::reactive_agent::CHAT_THREAD))
            .unwrap_or_default()
    }

    /// Replace the reactive-agent conversation thread with `messages`.
    ///
    /// The history-seeding hook a rebuilt kernel uses so the next turn streams
    /// against exactly those turns. A no-op if the store mutex is poisoned.
    pub fn seed_history(&self, messages: Vec<Message>) {
        if let Ok(mut store) = self.conv_store.lock() {
            store.replace_thread(
                sven_machines::machines::reactive_agent::CHAT_THREAD,
                messages,
            );
        }
    }

    /// The live tool registry (for MCP tool hot-swap).
    #[must_use]
    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.tool_registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::ApprovalId;

    /// When the sub-agent's request is given up (the session that sent it
    /// ended), the gate sees its reply channel close and can withdraw the
    /// prompt; and the prompt shows the call it gates and what it does.
    #[tokio::test]
    async fn a_given_up_sub_agent_request_is_withdrawn_from_the_gate() {
        use sven_tool_api::PermissionRequester as _;
        let (approvals, mut gate) = mpsc::channel(4);
        let approver = GateApprover::new(approvals);
        let call = sven_tool_api::ToolCall {
            id: "c".into(),
            name: "shell".into(),
            args: serde_json::json!({"command": "make"}),
        };
        let mut asking =
            Box::pin(approver.request_permission(&call, sven_hsm::ToolCapability::ExecuteShell));
        let mut request = tokio::select! {
            request = gate.recv() => request.expect("the gate is asked"),
            _ = &mut asking => panic!("answered before anyone was asked"),
        };
        assert_eq!(request.capability, sven_hsm::ToolCapability::ExecuteShell);
        assert_eq!(
            request.call.as_ref().map(|c| c.args["command"].clone()),
            Some("make".into())
        );
        drop(asking);
        tokio::time::timeout(std::time::Duration::from_secs(2), request.reply_tx.closed())
            .await
            .expect("the prompt is withdrawn");
    }

    /// A session nobody is at never waits for a person: a question is
    /// answered at once, saying that no user is available.
    #[tokio::test]
    async fn an_unattended_question_is_answered_at_once_with_the_no_user_answer() {
        let (question_tx, question_rx) = mpsc::channel(4);
        let (approval_tx, approval_rx) = mpsc::channel(4);
        tokio::spawn(
            KernelChannels {
                question_rx,
                approval_rx,
            }
            .answer_unattended(),
        );
        let (reply_tx, reply_rx) = oneshot::channel();
        question_tx
            .send(UserQuestion {
                prompt: "which database?".into(),
                reply_tx,
            })
            .await
            .expect("queued");
        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
            .await
            .expect("answered without waiting for anyone")
            .expect("a reply");
        assert_eq!(answer, sven_tool_api::NO_USER_ANSWER);

        let (reply_tx, reply_rx) = oneshot::channel();
        approval_tx
            .send(ApprovalRequest {
                approval_id: sven_hsm::ApprovalId::new(),
                capability: ToolCapability::ExecuteShell,
                description: "rm -rf build".into(),
                call: None,
                reply_tx,
            })
            .await
            .expect("queued");
        let approved = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
            .await
            .expect("answered at once")
            .expect("a reply");
        assert!(!approved, "nobody approves on an absent person's behalf");
    }

    /// The seam's whole reason for existing: the host decides, and it may
    /// take as long as a person does. The responder here holds both reply
    /// channels and answers only after both gates have arrived, which a
    /// synchronous answerer could not do.
    #[tokio::test]
    async fn forward_to_hands_both_gate_kinds_over_and_the_host_answers_when_it_likes() {
        let (question_tx, question_rx) = mpsc::channel(4);
        let (approval_tx, approval_rx) = mpsc::channel(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };

        let held: Arc<std::sync::Mutex<Vec<HumanGate>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let sink = Arc::clone(&held);
        let forwarding = tokio::spawn(
            channels.forward_to(Arc::new(move |gate| sink.lock().unwrap().push(gate))),
        );

        let (q_reply_tx, q_reply_rx) = oneshot::channel();
        question_tx
            .send(UserQuestion {
                prompt: "which account?".into(),
                reply_tx: q_reply_tx,
            })
            .await
            .expect("queued");
        let (a_reply_tx, a_reply_rx) = oneshot::channel();
        approval_tx
            .send(ApprovalRequest {
                approval_id: ApprovalId::new(),
                capability: ToolCapability::ReadFile,
                description: "delete the account".into(),
                call: None,
                reply_tx: a_reply_tx,
            })
            .await
            .expect("queued");

        // Both gates reach the host before either is answered.
        let gates = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if held.lock().unwrap().len() == 2 {
                    return std::mem::take(&mut *held.lock().unwrap());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both gates forwarded");

        for gate in gates {
            match gate {
                HumanGate::Question { prompt, reply_tx } => {
                    assert_eq!(prompt, "which account?");
                    reply_tx.send("the second one".into()).expect("answered");
                }
                HumanGate::Approval {
                    prompt, reply_tx, ..
                } => {
                    assert_eq!(prompt, "delete the account");
                    reply_tx.send(false).expect("answered");
                }
            }
        }

        assert_eq!(q_reply_rx.await.expect("a reply"), "the second one");
        assert!(
            !a_reply_rx.await.expect("a reply"),
            "the host's DENIAL is what reaches the kernel -- the point of this \
             seam is that no default is substituted for it"
        );

        drop(question_tx);
        drop(approval_tx);
        forwarding.await.expect("returns once both channels close");
    }
}
