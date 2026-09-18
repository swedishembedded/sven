// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One agent: a conversation, its kernel state, and the engine it borrows.

use sven_bootstrap::RuntimeBuilder;
use sven_hsm::{Event, UiEvent};
use sven_model::Message;
use sven_session_model::reduce_history;
use sven_vocab::SessionEvent;
use tokio::sync::broadcast;

use crate::engine::{ApprovalPolicy, Engine};
use crate::error::CallError;
use crate::state::AgentState;

/// Capacity of the per-agent event broadcast channel.
const EVENT_CAPACITY: usize = 1024;

/// A live agent: a conversation in progress against an [`Engine`].
///
/// Cheap to create and cheap to drop. Everything expensive belongs to the
/// engine; everything durable belongs to the [`AgentState`] this can be
/// suspended into. A service typically creates one per request, advances it by
/// a step, and puts it back.
pub struct Agent {
    engine: Engine,
    state: AgentState,
    events: broadcast::Sender<SessionEvent>,
}

impl Agent {
    pub(crate) fn new(engine: Engine, state: AgentState) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            engine,
            state,
            events,
        }
    }

    /// Subscribes to everything this agent emits while it works.
    ///
    /// The same [`SessionEvent`] stream the TUI and the headless runner
    /// consume, so a custom surface renders progress without reaching into any
    /// kernel crate. Subscribe before calling [`Agent::send`]; a receiver
    /// created afterwards sees only what is still buffered.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// The agent's current state, including its history.
    #[must_use]
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Suspends the agent, yielding the state needed to resume it later.
    #[must_use]
    pub fn suspend(self) -> AgentState {
        self.state
    }

    /// Sends `text` to the agent and runs one turn, returning its reply.
    ///
    /// The turn's history and ending kernel state are folded back into this
    /// agent, so a subsequent `send` - or a `send` after a suspend/resume
    /// round trip - continues the same conversation.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if the agent's mode is not
    /// registered, and [`CallError::Infrastructure`] if the kernel session
    /// cannot be built or the event queue closes mid-turn. A model that simply
    /// answers badly is not an error - it is the reply.
    pub async fn send(&mut self, text: &str) -> Result<String, CallError> {
        let registry = sven_machines::ModeRegistry::default_registry();
        if registry.get(&self.state.mode).is_none() {
            return Err(CallError::Precondition(format!(
                "unknown mode {:?}; this engine can run {:?}",
                self.state.mode,
                registry.modes()
            )));
        }

        let mut builder = RuntimeBuilder::new(self.engine.config(), self.state.mode.clone())
            .with_allow_interactive_oauth(false)
            .with_initial_history(self.state.history.clone());
        if let Some(provider) = self.engine.provider() {
            builder = builder.with_shared_model_provider(provider);
        }
        if let Some(snapshot) = self.state.kernel.clone() {
            builder = builder.with_kernel_snapshot(snapshot);
        }

        let bundle = builder.build_session().await?;

        match self.engine.approvals() {
            ApprovalPolicy::AutoApprove => {
                tokio::spawn(bundle.channels.auto_approve());
            }
            ApprovalPolicy::Deny => {
                tokio::spawn(bundle.channels.deny_all());
            }
        }

        let mut observations = bundle.handle.subscribe_observations();
        self.state.history.push(Message::user(text));

        if !bundle
            .handle
            .sink()
            .emit(Event::UserMessage {
                text: text.to_string(),
            })
            .await
        {
            return Err(CallError::Infrastructure(anyhow::anyhow!(
                "kernel event queue closed before the message was delivered"
            )));
        }

        let reply = self.drain_turn(&mut observations).await;

        // Capture where the kernel ended up before the session is torn down,
        // so the next step resumes here instead of re-entering from the top.
        self.state.kernel = bundle.runtime.capture().await;
        Ok(reply)
    }

    /// Consumes observations until the turn ends, folding them into history
    /// and republishing them to this agent's subscribers.
    async fn drain_turn(&mut self, observations: &mut broadcast::Receiver<UiEvent>) -> String {
        let mut reply = String::new();
        loop {
            match observations.recv().await {
                Ok(event) => {
                    let done = matches!(
                        event,
                        SessionEvent::TurnComplete | SessionEvent::Aborted { .. }
                    );
                    match &event {
                        SessionEvent::TextComplete(text) => reply.push_str(text),
                        SessionEvent::Aborted { partial_text } => reply.push_str(partial_text),
                        _ => {}
                    }
                    reduce_history(&event, &mut self.state.history);
                    let _ = self.events.send(event);
                    if done {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
        reply
    }
}
