// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for running agent sessions on a
// person's own machine for a server for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! The requests a session's host carries out on the session itself rather
//! than sending them as a message: `/compact` and `/goal`.

use sven_control::SessionState;
use sven_frontend::RequestError;
use sven_vocab::goal::{Goal, GoalCommand};

use super::SessionActor;

impl SessionActor {
    /// `/compact`: the session summarises its conversation now. The answer
    /// is the kernel's own: `ContextCompacted` with the summary, or a warning
    /// when there is nothing to summarise, then `TurnComplete`. What the
    /// kernel cannot answer is answered here, so the server never waits for
    /// an ending that will not come: a turn still running (the request is
    /// refused, never queued) and a mode that cannot compact.
    pub(super) async fn compact(&mut self) {
        if self.working {
            self.reply("Cannot compact now: a turn is running. Stop it or wait for it to finish.")
                .await;
            return;
        }
        if let Err(e) = self.start_compaction().await {
            self.fail(&format!("{e:#}")).await;
        }
    }

    async fn start_compaction(&mut self) -> anyhow::Result<()> {
        self.ensure_controller().await?;
        self.state(SessionState::Running).await;
        self.expect_ending();
        let Some(controller) = &self.controller else {
            return Ok(());
        };
        match controller.compact().await {
            Ok(()) => Ok(()),
            Err(e @ RequestError::Unsupported { .. }) => {
                self.reply(&format!("{e}.")).await;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Opens the live session if there is none, on the conversation kept.
    async fn ensure_controller(&mut self) -> anyhow::Result<()> {
        if self.controller.is_none() {
            let model = self.model_config()?;
            self.controller = Some(self.open_controller(model).await?);
        }
        Ok(())
    }

    /// `/goal ...`: hands the command to the session. At rest the answer is
    /// the session's own (the goal's announcement and its end); a command
    /// while a turn runs only changes what follows it.
    pub(super) async fn goal_command(&mut self, command: GoalCommand) {
        // The same goal asked for again - the server repeating the request a
        // stopped host was working on, or the person - continues it instead
        // of starting it over.
        let command = match (&command, &self.goal) {
            (GoalCommand::Set { objective, .. }, Some(kept))
                if kept.objective == objective.trim()
                    && (kept.status.is_resumable() || kept.status.is_active()) =>
            {
                GoalCommand::Resume
            }
            _ => command,
        };
        if let Err(e) = self.ensure_controller().await {
            self.fail(&format!("{e:#}")).await;
            return;
        }
        let at_rest = !self.working;
        if at_rest {
            self.state(SessionState::Running).await;
            self.expect_ending();
        }
        let Some(controller) = &self.controller else {
            return;
        };
        match controller.goal(command, at_rest).await {
            // At rest the session answers it; during a turn the running
            // answer carries what the goal became, and this one says so.
            Ok(()) if at_rest => {}
            Ok(()) => {
                self.reply("The goal is changed; the running answer goes on.")
                    .await
            }
            Err(e @ RequestError::Unsupported { .. }) => self.reply(&format!("{e}.")).await,
            Err(e) => self.fail(&format!("{e:#}")).await,
        }
    }

    /// The goal as it stands: the running session's, else the one kept for
    /// it (a session this run has not opened).
    pub(super) fn current_goal(&self) -> Option<Goal> {
        match &self.controller {
            Some(controller) => controller.current_goal(),
            None => self
                .goal
                .clone()
                .or_else(|| self.context.sessions.load_goal(self.id)),
        }
    }

    /// Keeps the goal the session announced, beside its conversation.
    pub(super) fn keep_goal(&mut self, goal: Option<Goal>) {
        if self.closed {
            return;
        }
        if let Err(e) = self.context.sessions.save_goal(self.id, goal.as_ref()) {
            tracing::warn!("session {}: could not keep its goal: {e:#}", self.id);
        }
        self.goal = goal;
    }
}
