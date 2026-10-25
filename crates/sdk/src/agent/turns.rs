// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The ways a turn starts: a message, an answer, a decision, or a
//! continuation of the conversation an agent already has.
//!
//! A fork of a recorded transcript continues it with
//! [`Agent::continue_with`]: the model is asked where the recorded
//! conversation stopped, with no new user turn invented between them.

use sven_hsm::ApprovalId;

use super::Agent;
use crate::error::CallError;
use crate::run::{Decision, RunOptions, RunOutcome};
use crate::run_support::{Entry, TurnOptions};

impl Agent {
    /// Sends `text` to the agent and runs one turn with no bounds beyond the
    /// engine's own. See [`Self::send_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::send_with`].
    pub async fn send(&mut self, text: &str) -> Result<RunOutcome, CallError> {
        self.send_with(text, RunOptions::default()).await
    }

    /// Sends `text` to the agent and runs one turn within `bounds`, returning
    /// how it ended, the reply and the tokens it used.
    ///
    /// The turn's history and ending kernel state are folded back into this
    /// agent, so a subsequent `send` - or a `send` after a suspend/resume
    /// round trip - continues the same conversation, also after a run that
    /// was cancelled or stopped by a bound.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if the agent's mode is not
    /// registered, and [`CallError::Infrastructure`] if the kernel session
    /// cannot be built, the event queue closes mid-turn, or the turn itself
    /// failed (the provider errored). A model that simply answers badly is
    /// not an error - it is the reply - and a run stopped by one of `bounds`
    /// is an outcome, not an error.
    pub async fn send_with(
        &mut self,
        text: &str,
        bounds: RunOptions,
    ) -> Result<RunOutcome, CallError> {
        self.run(
            Entry::Message(text.to_string()),
            &TurnOptions::default(),
            &bounds,
        )
        .await
    }

    /// Runs one turn that continues the conversation the agent already has,
    /// without adding a user message.
    ///
    /// This is how an agent forked from a recorded transcript
    /// ([`Engine::fork`](crate::Engine::fork)) carries on where the recorded
    /// session stopped: the model's next turn is asked on exactly that
    /// conversation, so no synthetic prompt appears in what it reads or in
    /// the trajectory it produces. The agent's recorded role, when it has
    /// one, is the system prompt of the continuation.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the conversation is empty (there is
    /// nothing to continue), and otherwise as [`Self::send_with`].
    pub async fn continue_with(&mut self, bounds: RunOptions) -> Result<RunOutcome, CallError> {
        if self.state.history.is_empty() {
            return Err(CallError::Precondition(
                "a continuation needs a conversation to continue".into(),
            ));
        }
        let options = TurnOptions {
            role: self.state.role.clone(),
            ..TurnOptions::default()
        };
        self.run(Entry::Continue, &options, &bounds).await
    }

    /// Answers the question the agent is waiting on and continues the run.
    /// See [`Self::answer_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::answer_with`].
    pub async fn answer(&mut self, question_id: &str, text: &str) -> Result<RunOutcome, CallError> {
        self.answer_with(question_id, text, RunOptions::default())
            .await
    }

    /// Answers the question a run ended [`RunConclusion::Waiting`](crate::RunConclusion::Waiting) on, and
    /// continues that run within `bounds`.
    ///
    /// The answer becomes the result of the tool call that asked, so the
    /// model reads it exactly as if the question had been answered on the
    /// spot. Works on a resumed agent as well as on the one that asked.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if the agent is not waiting on
    /// `question_id`, and otherwise fails as [`Self::send_with`].
    pub async fn answer_with(
        &mut self,
        question_id: &str,
        text: &str,
        bounds: RunOptions,
    ) -> Result<RunOutcome, CallError> {
        let pending = self
            .state
            .kernel
            .as_ref()
            .and_then(|snapshot| snapshot.context.pending_question.as_ref())
            .filter(|q| q.question_id.as_uuid().to_string() == question_id)
            .ok_or_else(|| {
                CallError::Precondition(format!(
                    "the agent is not waiting on question {question_id:?}"
                ))
            })?;
        if pending.call_ref.is_empty() {
            return Err(CallError::Precondition(format!(
                "question {question_id:?} was parked without the id of the call that asked, \
                 so its answer has nowhere to go"
            )));
        }
        let entry = Entry::Answer {
            question_id: pending.question_id,
            call_ref: pending.call_ref.clone(),
            answer: text.to_string(),
        };
        self.run(entry, &TurnOptions::default(), &bounds).await
    }

    /// Decides the approval the agent is parked on and continues the run. See
    /// [`Self::decide_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::decide_with`].
    pub async fn decide(
        &mut self,
        approval_id: ApprovalId,
        decision: Decision,
    ) -> Result<RunOutcome, CallError> {
        self.decide_with(approval_id, decision, RunOptions::default())
            .await
    }

    /// Decides the approval a run ended [`RunConclusion::Waiting`](crate::RunConclusion::Waiting) on - see
    /// [`EngineBuilder::park_approvals`](crate::EngineBuilder::park_approvals) -
    /// and continues that run within `bounds`.
    ///
    /// An approved call runs exactly as the model proposed it; a denied one
    /// does not run, and the model reads the reason. Works on a resumed agent
    /// as well as on the one that asked, and the run keeps the [`RunId`](crate::RunId) it
    /// had. An approval is decided once.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if the agent is not waiting on
    /// `approval_id`, and otherwise fails as [`Self::send_with`].
    pub async fn decide_with(
        &mut self,
        approval_id: ApprovalId,
        decision: Decision,
        bounds: RunOptions,
    ) -> Result<RunOutcome, CallError> {
        if self.pending_approval().map(|a| a.id) != Some(approval_id) {
            return Err(CallError::Precondition(format!(
                "the agent is not waiting on approval {approval_id}"
            )));
        }
        let entry = Entry::Decision {
            approval_id,
            decision,
        };
        self.run(entry, &TurnOptions::default(), &bounds).await
    }
}
