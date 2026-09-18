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

use tokio::sync::{mpsc, watch};

use sven_executors::{ApprovalRequest, UserQuestion};
use sven_hsm::{Event, RuntimeStatus};
use sven_kernel::EventSink;
use sven_llm::ThreadStore;
use sven_model::Message;
use sven_tools::ToolRegistry;

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
    /// Auto-consumes every kernel-level question and approval gate, replying
    /// immediately so the session never blocks on a human who isn't there:
    /// an empty string for every `AskUser`, `true` (approve) for every
    /// `RequestHumanApproval`. Returns once both channels close.
    ///
    /// This is the unattended path -- CI runs and one-shot test/demo
    /// wiring. Typically
    /// driven with `tokio::spawn(channels.auto_approve())`.
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

/// A cheap-to-clone handle to a spawned [`ErasedRuntime`].
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
