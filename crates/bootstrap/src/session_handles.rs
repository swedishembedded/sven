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

impl KernelChannels {
    /// Hands every kernel-level question and approval gate to `responder`,
    /// which owns replying to each.
    ///
    /// The third option beside [`Self::auto_approve`] and [`Self::deny_all`],
    /// and the only one that is not a decision made on the absent person's
    /// behalf. Returns once both channels close.
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

    /// Refuses every kernel-level question and approval gate, replying
    /// immediately so the session never blocks on a human who isn't there:
    /// an empty string for every `AskUser`, `false` (deny) for every
    /// `RequestHumanApproval`. Returns once both channels close.
    ///
    /// The counterpart to [`Self::auto_approve`], and the safe default for an
    /// unattended session: answering the gate is mandatory - a turn that
    /// ignores it hangs - but answering it with "yes" hands a dangerous
    /// capability to nobody's judgement.
    pub async fn deny_all(mut self) {
        loop {
            tokio::select! {
                q = self.question_rx.recv() => match q {
                    Some(q) => { let _ = q.reply_tx.send(String::new()); }
                    None => break,
                },
                a = self.approval_rx.recv() => match a {
                    Some(a) => { let _ = a.reply_tx.send(false); }
                    None => break,
                },
            }
        }
    }

    /// Auto-consumes every kernel-level question and approval gate, replying
    /// immediately so the session never blocks on a human who isn't there:
    /// an empty string for every `AskUser`, `true` (approve) for every
    /// `RequestHumanApproval`. Returns once both channels close.
    ///
    /// This is the unattended path - CI runs and one-shot test/demo wiring.
    /// Typically driven with `tokio::spawn(channels.auto_approve())`.
    ///
    /// **Prefer [`Self::forward_to`] whenever the host CAN answer.** A host
    /// with a person attached that calls this is deciding on their behalf
    /// without telling them.
    pub async fn auto_approve(mut self) {
        loop {
            tokio::select! {
                q = self.question_rx.recv() => match q {
                    Some(q) => { let _ = q.reply_tx.send(String::new()); }
                    None => break,
                },
                a = self.approval_rx.recv() => match a {
                    Some(a) => { let _ = a.reply_tx.send(true); }
                    None => break,
                },
            }
        }
    }
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
