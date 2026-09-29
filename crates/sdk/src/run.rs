// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The bounds a run is given and the account it gives back.

use std::time::Duration;

use tokio::sync::watch;

pub use sven_session_model::RunConclusion;

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
    #[must_use]
    pub fn deadline(mut self, after: Duration) -> Self {
        self.deadline = Some(after);
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

/// Tokens the provider reported for a run. `None` means the provider
/// reported nothing for that count, never that it was zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Prompt tokens processed, excluding prompt-cache hits.
    pub input_tokens: Option<u64>,
    /// Tokens the model generated.
    pub output_tokens: Option<u64>,
}

impl Usage {
    /// Adds one usage report. A zero field is the provider not reporting it.
    pub(crate) fn add(&mut self, input: u32, output: u32) {
        let add = |slot: &mut Option<u64>, n: u32| {
            if n > 0 {
                *slot = Some(slot.unwrap_or(0) + u64::from(n));
            }
        };
        add(&mut self.input_tokens, input);
        add(&mut self.output_tokens, output);
    }
}

/// How a run ended and what it produced.
#[derive(Clone, Debug, PartialEq)]
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
}
