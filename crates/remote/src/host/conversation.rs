// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for running agent sessions on a
// person's own machine for a server for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! The conversation a session holds: a message submitted to it, the turns
//! the server took back, and clearing it.

use sven_control::SessionState;
use sven_machines::AgentEvent;
use sven_model::Message;
use sven_vocab::AgentMode;
use tokio::sync::mpsc;

use super::{attachments, SessionActor};

impl SessionActor {
    /// Takes turns back: the conversation becomes exactly `history`, the
    /// server's flat text (tool calls of the turns kept are lost, as for a
    /// session the host never ran). A turn still running is cancelled and the
    /// live session let go; the next input opens a new one on `history`. The
    /// kept copy is replaced too, so a restart does not bring the old turns
    /// back. Events the cancelled turn had not delivered yet are dropped: they
    /// would otherwise end the answer started next.
    pub(super) async fn replace_conversation(&mut self, mode: AgentMode, history: Vec<Message>) {
        if let Some(controller) = self.controller.take() {
            controller.cancel().await;
        }
        let (events_tx, events_rx) = mpsc::channel(1024);
        self.events_tx = events_tx;
        self.events_rx = events_rx;
        self.answers.forget();
        self.held_notices.clear();
        // The turns they waited behind are taken back with the conversation:
        // their answers end, empty.
        for queued in std::mem::take(&mut self.queued) {
            self.upload(AgentEvent::TurnComplete, queued.answer_id)
                .await;
        }
        if self.working {
            self.state(SessionState::Idle).await;
        }
        self.mode = mode;
        self.unfinished = None;
        self.history = history;
        if self.closed {
            return;
        }
        if let Err(e) = self.context.sessions.save(self.id, &self.history) {
            tracing::warn!(
                "session {}: could not keep its replaced conversation: {e:#}",
                self.id
            );
        }
    }
    /// Posts the person's input: its text, and with it the images the model
    /// takes, as one message.
    pub(super) async fn submit(&mut self, input: attachments::Attached) -> anyhow::Result<()> {
        let model = self.model_config()?;
        self.state(SessionState::Running).await;
        self.expect_ending();
        if self.controller.is_none() && !input.images.is_empty() {
            self.controller = Some(self.open_controller(model.clone()).await?);
        }
        match &mut self.controller {
            Some(controller) if !input.images.is_empty() => {
                let mut parts = vec![sven_model::ContentPart::text(input.text)];
                parts.extend(input.images);
                controller
                    .submit_parts(parts, Some(self.mode), Some(model))
                    .await?
            }
            Some(controller) => {
                controller
                    .submit(input.text, Some(self.mode), Some(model))
                    .await?
            }
            None => {
                let controller = self.open_controller(model).await?;
                controller.post(input.text).await?;
                self.controller = Some(controller);
            }
        }
        Ok(())
    }
    /// Starts the session over with no conversation: the live one is let go
    /// and its kept copy emptied, so a restart does not bring it back. Its
    /// goal goes with it.
    pub(super) fn clear(&mut self) {
        self.controller = None;
        self.held_notices.clear();
        self.history.clear();
        self.unfinished = None;
        self.keep_goal(None);
        if let Err(e) = self.context.sessions.save(self.id, &[]) {
            tracing::warn!(
                "session {}: could not clear its kept conversation: {e:#}",
                self.id
            );
        }
    }
}
