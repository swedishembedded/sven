// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The bounds a run is given and the account it gives back.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sven_hsm::{ApprovalId, GatedCall, ToolCapability};
use tokio::sync::watch;

pub use sven_session_model::RunConclusion;

/// Names one run: from the message (or call) that starts it to the way it
/// ends. A run that parks on a question or an approval and is continued by
/// [`Agent::answer`](crate::Agent::answer) or
/// [`Agent::decide`](crate::Agent::decide) is still the same run, also after a
/// suspend and resume in another process, so an event and a gate can always be
/// traced to the work that raised them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(uuid::Uuid);

impl RunId {
    /// A fresh identifier, distinct from every other.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The identifier as a UUID, for a log or a key.
    #[must_use]
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// The identifier a harness run goes by: its own id when that is a UUID,
    /// otherwise a fresh one.
    pub(crate) fn from_text(text: &str) -> Self {
        uuid::Uuid::parse_str(text).map_or_else(|_| Self::new(), Self)
    }

    /// A placeholder for the rare event that cannot name its run.
    pub(crate) fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Stops a run from outside. Cheap to clone; every clone cancels the same
/// runs, and a token once cancelled stays cancelled.
#[derive(Clone, Debug)]
pub struct CancelToken {
    fired: std::sync::Arc<watch::Sender<bool>>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// A token that has not fired.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fired: std::sync::Arc::new(watch::channel(false).0),
        }
    }

    /// Cancels every run holding this token.
    pub fn cancel(&self) {
        self.fired.send_replace(true);
    }

    /// Whether [`Self::cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.fired.borrow()
    }

    /// Completes once the token is cancelled.
    pub(crate) async fn cancelled(&self) {
        let mut rx = self.fired.subscribe();
        // The sender lives as long as `self`, so the wait cannot fail.
        let _ = rx.wait_for(|fired| *fired).await;
    }
}

/// The bounds of one run. Every bound is off unless set; the engine's
/// configured tool-round budget applies regardless.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub(crate) cancel: Option<CancelToken>,
    pub(crate) deadline: Option<Duration>,
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) decoding: sven_turn::Decoding,
}

impl RunOptions {
    /// No bounds beyond the engine's own.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops the run when `token` is cancelled ([`RunConclusion::Cancelled`]).
    #[must_use]
    pub fn cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Stops the run once `after` has elapsed ([`RunConclusion::Timeout`]).
    ///
    /// The SDK sets no deadline by default, so a gate handler that never
    /// replies holds a run without one; a model server that goes quiet is
    /// ended by the stall limits (`agent.first_byte_timeout_secs` and
    /// friends) whether or not a deadline is set.
    #[must_use]
    pub fn deadline(mut self, after: Duration) -> Self {
        self.deadline = Some(after);
        self
    }

    /// Sends these sampling controls (temperature, top-p, top-k, seed) with
    /// every model request of the run, children's included. A provider that
    /// cannot honour one refuses the request rather than ignoring it.
    #[must_use]
    pub fn sampling(mut self, sampling: sven_model::SamplingParams) -> Self {
        self.decoding.sampling = sampling;
        self
    }

    /// Asks the provider for the sampled token ids and log-probabilities, so
    /// the trajectory can serve as a training record.
    #[must_use]
    pub fn want_logprobs(mut self, wanted: bool) -> Self {
        self.decoding.want_logprobs = wanted;
        self
    }

    /// Stops the run once the model has generated `tokens` output tokens
    /// ([`RunConclusion::BudgetExhausted`]). Counted from the usage the
    /// provider reports; a provider that reports none is never stopped by it.
    #[must_use]
    pub fn max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }
}

/// What a run used. A token or cost count of `None` means the provider
/// reported nothing for it, never that it was zero.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Usage {
    /// Prompt tokens processed, excluding prompt-cache hits.
    pub input_tokens: Option<u64>,
    /// Tokens the model generated.
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache.
    pub cached_tokens: Option<u64>,
    /// What the provider charged, in USD, when it says.
    pub cost_usd: Option<f64>,
    /// Tool calls that ran to a result.
    pub tool_calls: u64,
    /// Of those, the ones whose result was an error.
    pub tool_errors: u64,
    /// Model requests made.
    pub turns: u64,
    /// Time from the start of the run to its end.
    pub wall_time: Duration,
}

/// A question the agent is waiting on, reported with
/// [`RunConclusion::Waiting`] and answered with
/// [`Agent::answer`](crate::Agent::answer).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Question {
    /// Identifies the question to [`Agent::answer`](crate::Agent::answer).
    pub id: String,
    /// What the agent asked.
    pub prompt: String,
    /// The choices it offered, if any; empty for a free-form question.
    pub options: Vec<String>,
}

impl Question {
    pub(crate) fn from_pending(pending: &sven_hsm::context::PendingQuestion) -> Self {
        Self {
            id: pending.question_id.as_uuid().to_string(),
            prompt: pending.prompt.clone(),
            options: pending.options.clone(),
        }
    }
}

/// An approval that was parked, reported with
/// [`RunConclusion::Waiting`] and decided with
/// [`Agent::decide`](crate::Agent::decide). See
/// [`EngineBuilder::park_approvals`](crate::EngineBuilder::park_approvals).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Approval {
    /// Identifies the approval to [`Agent::decide`](crate::Agent::decide). An
    /// approval is for one call: the next call of the same kind is asked
    /// about again.
    pub id: ApprovalId,
    /// The run that raised it, and that deciding it continues.
    pub run_id: RunId,
    /// What kind of effect the call has.
    pub capability: ToolCapability,
    /// What will happen if this is approved.
    pub prompt: String,
    /// The tool call it gates, with its arguments exactly as the model
    /// proposed them; `None` for a decision a machine puts to a person.
    pub call: Option<GatedCall>,
}

impl Approval {
    pub(crate) fn from_pending(
        pending: &sven_hsm::context::PendingApproval,
        run_id: RunId,
        context: &sven_hsm::Context,
    ) -> Self {
        let proposed = sven_machines::machines::loop_core::LoopState::load(context).proposed;
        let call = pending
            .call_id
            .and_then(|call_id| proposed.get(&call_id))
            .map(|call| GatedCall {
                name: call.name.clone(),
                args: call.args.clone(),
            });
        Self {
            id: pending.approval_id,
            run_id,
            capability: pending.capability,
            prompt: pending.description.clone(),
            call,
        }
    }
}

/// What a person decided about a parked [`Approval`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the call run, exactly as the model proposed it.
    Approve,
    /// Refuse the call. The model reads `reason` as the reason it did not
    /// run.
    Deny {
        /// Why the call was refused.
        reason: String,
    },
}

/// How a run ended and what it produced.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RunOutcome {
    /// Why the run stopped. [`RunConclusion::Success`] is the only ending in
    /// which the agent finished its turn on its own terms;
    /// [`RunConclusion::BudgetExhausted`] covers both a spent output-token
    /// budget and a turn wrapped up because the tool-round budget ran out.
    pub conclusion: RunConclusion,
    /// The agent's reply, or what it had said when the run was stopped.
    pub reply: String,
    /// Tokens used by this run.
    pub usage: Usage,
    /// The question the agent is waiting on when the run ended
    /// [`RunConclusion::Waiting`]; `None` otherwise.
    pub question: Option<Question>,
    /// The approval the agent is waiting on when the run ended
    /// [`RunConclusion::Waiting`] under
    /// [`EngineBuilder::park_approvals`](crate::EngineBuilder::park_approvals);
    /// `None` otherwise.
    pub approval: Option<Approval>,
}
