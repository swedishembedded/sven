// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervised, long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `/goal` in the terminal: the command goes to the session's kernel, its
//! answer (`GoalUpdated`) is kept on the session, which saves it with the
//! conversation, and a session opened again gets its goal back.

use sven_frontend::agent::AgentRequest;
use sven_machines::AgentEvent;
use sven_model::Message;
use sven_vocab::goal::{GoalCommand, GoalUpdate};
use sven_vocab::AgentMode;

use crate::app::App;
use crate::chat::segment::ChatSegment;

impl App {
    /// Hands `command` to the active session's agent. At rest the session
    /// is busy until the answer ends (`TurnComplete`): at once for a command
    /// that starts no work, at the goal's end for one that does.
    pub(crate) async fn goal_command(&mut self, command: GoalCommand) {
        if self.session.mode == AgentMode::Sdlc {
            self.agent.notice =
                Some("warning: the sdlc mode keeps no goal; switch with /mode agent first".into());
            return;
        }
        let Some(tx) = self.agent.tx.clone() else {
            return;
        };
        if let GoalCommand::Set { objective, .. } = &command {
            if !self.agent.busy {
                self.chat
                    .push_segment(ChatSegment::Message(Message::user(format!(
                        "/goal {objective}"
                    ))));
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
        }
        if tx.send(AgentRequest::Goal(command)).await.is_err() {
            self.agent.notice = Some("warning: the agent session has stopped".into());
            return;
        }
        self.agent.busy = true;
    }

    /// `/goal` alone: the goal the active session last reported.
    pub(crate) fn show_goal(&mut self) {
        let goal = self
            .sessions
            .get(&self.sessions.active_id)
            .and_then(|entry| entry.goal.clone());
        self.show_notice(&sven_frontend::goal::describe(goal.as_ref()));
    }

    /// The active session's goal changed: keep it, say what happened, save.
    pub(crate) fn goal_updated(&mut self, update: GoalUpdate) {
        let active = self.sessions.active_id.clone();
        if let Some(entry) = self.sessions.get_mut(&active) {
            entry.apply_background_event(&AgentEvent::GoalUpdated(Box::new(update.clone())));
        }
        self.agent.notice = Some(format!("goal: {}", update.note));
        self.save_history_async();
    }

    /// The request that puts the active session's kept goal back into a
    /// freshly started agent, if it has one.
    pub(crate) fn restore_goal_request(&self) -> Option<AgentRequest> {
        let goal = self.sessions.get(&self.sessions.active_id)?.goal.clone()?;
        Some(AgentRequest::Goal(GoalCommand::Restore {
            goal: Box::new(goal),
        }))
    }
}
