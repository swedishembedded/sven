// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running language-model agents
// within token and turn budgets that hold, so that agents can be compared at
// equal cost, for its clients. If your team needs expertise in budgeted agent
// evaluation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The budget of a run: the most completion (output) tokens and the most
//! completions every model request of this process may take together.
//!
//! [`install`] sets it once per process; from then on every provider
//! [`crate::from_config`] builds passes through [`BudgetedProvider`], which
//!
//! - refuses a request once the budget is spent, with [`BudgetExhausted`],
//!   and remembers that it did ([`RunBudget::exhausted`]), so a runner
//!   concludes the run from that flag, never from an error's text;
//! - counts every request it lets through as one turn, and lowers the
//!   request's output limit to the tokens still left;
//! - charges each completion the output tokens its stream reports; a stream
//!   that ended without reporting any is charged all it was allowed, with a
//!   warning; a stream its consumer cut off before its end (a watchdog that
//!   stopped looping reasoning, a cancel) is charged what it streamed: its
//!   sampled token ids when the provider returned them, else its text at
//!   about four characters a token.
//!
//! What a completion may produce is reserved while it streams, so two
//! requests in flight together cannot both be given the same remaining
//! tokens. Completions a delegated child makes in its own process are
//! charged with [`RunBudget::charge`] when the child reports them.

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};

/// What a run may spend. `None` is no bound.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBudgetLimits {
    /// Most completion (output) tokens, reasoning included, of the run.
    pub max_completion_tokens: Option<u64>,
    /// Most model completions of the run.
    pub max_turns: Option<u32>,
}

impl RunBudgetLimits {
    /// Whether anything is bounded.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.max_completion_tokens.is_some() || self.max_turns.is_some()
    }
}

/// What a run has spent: completion tokens and completions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spent {
    pub completion_tokens: u64,
    pub turns: u32,
}

impl Spent {
    /// What was spent between `earlier` and `self`.
    #[must_use]
    pub fn since(self, earlier: Spent) -> Spent {
        Spent {
            completion_tokens: self
                .completion_tokens
                .saturating_sub(earlier.completion_tokens),
            turns: self.turns.saturating_sub(earlier.turns),
        }
    }
}

/// The key under which a process that served a delegated turn reports, in
/// the turn's response metadata, the [`Spent`] the turn took of its budget,
/// so the delegating run can [`RunBudget::charge`] it.
pub const SPENT_META_KEY: &str = "sven.run_budget_spent";

/// The budget in force when a request was sent, as the request log records
/// it beside the request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InForce {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Completion tokens charged before this request was sent.
    pub completion_tokens_spent: u64,
    /// Which completion of the run this request is, counting from 1.
    pub turn: u32,
}

/// Which bound a refused request ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    CompletionTokens,
    Turns,
}

/// The error a request gets once the run's budget is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExhausted {
    pub bound: Bound,
    pub limits: RunBudgetLimits,
    pub spent: Spent,
}

impl fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "run budget exhausted: {}",
            describe(self.limits, self.spent)
        )
    }
}

impl std::error::Error for BudgetExhausted {}

/// `4096 of 4096 completion tokens, 12 of 40 turns`, naming only the bounds
/// that are set.
fn describe(limits: RunBudgetLimits, spent: Spent) -> String {
    let mut parts = Vec::new();
    if let Some(max) = limits.max_completion_tokens {
        parts.push(format!(
            "{} of {max} completion tokens",
            spent.completion_tokens
        ));
    }
    if let Some(max) = limits.max_turns {
        parts.push(format!("{} of {max} turns", spent.turns));
    }
    parts.join(", ")
}

#[derive(Default)]
struct State {
    spent: Spent,
    /// Output tokens allowed to completions still streaming.
    reserved: u64,
    /// The bound a request was refused at, once one was.
    refused: Option<Bound>,
    /// Completions that reported no usage and were charged all they were
    /// allowed.
    unmetered: u32,
    /// Completions cut off before their end, charged what they streamed.
    cut_off: u32,
}

/// A run's budget and what it has spent. See the module documentation.
pub struct RunBudget {
    limits: RunBudgetLimits,
    state: Mutex<State>,
}

/// One admitted completion: the turn it took and the tokens reserved for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Admission {
    reserved: u64,
}

impl RunBudget {
    #[must_use]
    pub fn new(limits: RunBudgetLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            state: Mutex::new(State::default()),
        })
    }

    #[must_use]
    pub fn limits(&self) -> RunBudgetLimits {
        self.limits
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What the run has spent so far, completions still streaming counted
    /// as turns but not yet as tokens.
    #[must_use]
    pub fn spent(&self) -> Spent {
        self.state().spent
    }

    /// Completions that reported no usage, each charged all it was allowed.
    /// A runner reports them: what such a run spent is an upper bound.
    #[must_use]
    pub fn unmetered(&self) -> u32 {
        self.state().unmetered
    }

    /// Completions their consumer cut off before their end, each charged what
    /// it streamed (an estimate when the provider returned no token ids).
    #[must_use]
    pub fn cut_off(&self) -> u32 {
        self.state().cut_off
    }

    /// The bound a request was refused at, if one was: the run is over.
    #[must_use]
    pub fn exhausted(&self) -> Option<BudgetExhausted> {
        let state = self.state();
        state.refused.map(|bound| BudgetExhausted {
            bound,
            limits: self.limits,
            spent: state.spent,
        })
    }

    /// What is left: completion tokens (none reserved by a completion still
    /// streaming) and turns. `None` where unbounded.
    #[must_use]
    pub fn remaining(&self) -> RunBudgetLimits {
        let state = self.state();
        RunBudgetLimits {
            max_completion_tokens: self.limits.max_completion_tokens.map(|max| {
                max.saturating_sub(state.spent.completion_tokens)
                    .saturating_sub(state.reserved)
            }),
            max_turns: self
                .limits
                .max_turns
                .map(|max| max.saturating_sub(state.spent.turns)),
        }
    }

    /// The budget in force for the request just admitted, as a recorder
    /// inside [`BudgetedProvider`] sees it.
    #[must_use]
    pub fn in_force(&self) -> InForce {
        let spent = self.spent();
        InForce {
            max_completion_tokens: self.limits.max_completion_tokens,
            max_turns: self.limits.max_turns,
            completion_tokens_spent: spent.completion_tokens,
            turn: spent.turns,
        }
    }

    /// Charges what was spent elsewhere on this run's behalf: a delegated
    /// child's completions. It may take the run past its bounds; the next
    /// request is then refused.
    pub fn charge(&self, spent: Spent) {
        let mut state = self.state();
        state.spent.completion_tokens = state
            .spent
            .completion_tokens
            .saturating_add(spent.completion_tokens);
        state.spent.turns = state.spent.turns.saturating_add(spent.turns);
    }

    /// Admits one more completion that would produce at most `wanted`
    /// tokens (`None`: as many as it is let), or refuses it. An admitted
    /// completion takes a turn and reserves the tokens it may produce.
    fn admit(&self, wanted: Option<u64>) -> Result<Admission, BudgetExhausted> {
        let mut state = self.state();
        let refuse = |state: &mut State, bound| {
            let bound = *state.refused.get_or_insert(bound);
            Err(BudgetExhausted {
                bound,
                limits: self.limits,
                spent: state.spent,
            })
        };
        if let Some(bound) = state.refused {
            return refuse(&mut state, bound);
        }
        if self
            .limits
            .max_turns
            .is_some_and(|max| state.spent.turns >= max)
        {
            return refuse(&mut state, Bound::Turns);
        }
        let reserved = match self.limits.max_completion_tokens {
            Some(max) => {
                let left = max
                    .saturating_sub(state.spent.completion_tokens)
                    .saturating_sub(state.reserved);
                if left == 0 {
                    return refuse(&mut state, Bound::CompletionTokens);
                }
                wanted.map_or(left, |w| w.min(left))
            }
            None => 0,
        };
        state.spent.turns += 1;
        state.reserved += reserved;
        Ok(Admission { reserved })
    }

    /// A request that never reached the model gives its turn and its
    /// reservation back.
    fn withdraw(&self, admission: Admission) {
        let mut state = self.state();
        state.spent.turns = state.spent.turns.saturating_sub(1);
        state.reserved = state.reserved.saturating_sub(admission.reserved);
    }

    /// Settles a completion: its reservation is released and it is charged
    /// what [`Charge`] says.
    fn settle(&self, admission: Admission, charge: Charge) {
        let charged = match charge {
            Charge::Reported(tokens) => {
                if self.limits.max_completion_tokens.is_some() && tokens > admission.reserved {
                    tracing::warn!(
                        allowed = admission.reserved,
                        reported = tokens,
                        "run budget: the model produced more completion tokens than it was \
                         allowed; the server did not honour the output limit"
                    );
                }
                tokens
            }
            Charge::Unmetered => {
                if self.limits.max_completion_tokens.is_some() {
                    tracing::warn!(
                        charged = admission.reserved,
                        "run budget: a completion reported no usage; it is charged all it was \
                         allowed"
                    );
                    self.state().unmetered += 1;
                }
                admission.reserved
            }
            Charge::CutOff(streamed) => {
                self.state().cut_off += 1;
                streamed.min(admission.reserved)
            }
        };
        let mut state = self.state();
        state.reserved = state.reserved.saturating_sub(admission.reserved);
        state.spent.completion_tokens = state.spent.completion_tokens.saturating_add(charged);
    }
}

/// What a completion is charged when its stream is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charge {
    /// The output tokens the stream reported.
    Reported(u64),
    /// The stream ended without reporting usage: all it was allowed.
    Unmetered,
    /// The consumer dropped the stream before its end: what it streamed.
    CutOff(u64),
}

/// The budget every provider of this process is held to, once [`install`]
/// set one.
static INSTALLED: OnceLock<Arc<RunBudget>> = OnceLock::new();

/// Holds every model request of this process to `limits` from now on:
/// [`crate::from_config`] and [`crate::from_config_probed`] wrap each
/// provider they build. Limits that bound nothing install nothing.
/// Installing twice is refused.
pub fn install(limits: RunBudgetLimits) -> Result<()> {
    if !limits.is_bounded() {
        return Ok(());
    }
    INSTALLED
        .set(RunBudget::new(limits))
        .map_err(|_| anyhow::anyhow!("a run budget is already installed"))
}

/// The budget this process is held to, if one was installed.
#[must_use]
pub fn installed() -> Option<&'static Arc<RunBudget>> {
    INSTALLED.get()
}

/// `provider`, held to the installed budget if there is one.
pub(crate) fn wrap(provider: Box<dyn ModelProvider>) -> Box<dyn ModelProvider> {
    match INSTALLED.get() {
        Some(budget) => Box::new(BudgetedProvider::new(
            Arc::from(provider),
            Arc::clone(budget),
        )),
        None => provider,
    }
}

/// A provider held to a [`RunBudget`]. See the module documentation.
pub struct BudgetedProvider {
    inner: Arc<dyn ModelProvider>,
    budget: Arc<RunBudget>,
}

impl BudgetedProvider {
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>, budget: Arc<RunBudget>) -> Self {
        Self { inner, budget }
    }

    /// The output limit `req` would be sent with: the one it carries, else
    /// the provider's own.
    fn wanted(&self, req: &CompletionRequest) -> Option<u64> {
        req.max_output_tokens_override
            .or_else(|| self.inner.config_max_output_tokens())
            .or_else(|| self.inner.catalog_max_output_tokens())
            .map(u64::from)
    }
}

/// Lowers `req`'s output limit to `allowed` when that is the tighter bound.
/// A limit that only stands for the room left in the context window stays
/// one while it is the tighter bound: the server ends such a reply at its
/// context.
fn clamp(req: &mut CompletionRequest, allowed: u64) {
    let allowed = u32::try_from(allowed).unwrap_or(u32::MAX);
    if req
        .max_output_tokens_override
        .is_none_or(|limit| limit > allowed)
    {
        req.max_output_tokens_override = Some(allowed);
        req.output_limit_is_room = false;
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ModelProvider for BudgetedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn complete(&self, mut req: CompletionRequest) -> Result<ResponseStream> {
        let admission = self.budget.admit(self.wanted(&req))?;
        if self.budget.limits.max_completion_tokens.is_some() {
            clamp(&mut req, admission.reserved);
        }
        let stream = match self.inner.complete(req).await {
            Ok(stream) => stream,
            Err(e) => {
                self.budget.withdraw(admission);
                return Err(e);
            }
        };
        let mut meter = Meter {
            budget: Arc::clone(&self.budget),
            admission,
            reported: None,
            ended: false,
            sampled: 0,
            streamed_chars: 0,
        };
        // The closure owns the whole meter (a method call on it, not a copy
        // of one field), so it settles when the stream is dropped.
        Ok(Box::pin(stream.map(move |item| {
            meter.observe(&item);
            item
        })))
    }

    async fn list_models(&self) -> Result<Vec<sven_model::ModelCatalogEntry>> {
        self.inner.list_models().await
    }

    async fn discover(&self) -> Option<sven_model::ServedModels> {
        self.inner.discover().await
    }

    fn take_model_change(&self) -> Option<sven_model::ModelChange> {
        self.inner.take_model_change()
    }

    fn tool_support(&self) -> Option<bool> {
        self.inner.tool_support()
    }

    fn supports_tool_choice_none(&self) -> bool {
        self.inner.supports_tool_choice_none()
    }

    fn sampling_controls(&self) -> sven_model::SamplingControls {
        self.inner.sampling_controls()
    }

    fn sampling_best_effort(&self) -> bool {
        self.inner.sampling_best_effort()
    }

    fn endpoint_host(&self) -> Option<String> {
        self.inner.endpoint_host()
    }

    fn is_local(&self) -> bool {
        self.inner.is_local()
    }

    fn catalog_max_output_tokens(&self) -> Option<u32> {
        self.inner.catalog_max_output_tokens()
    }

    fn catalog_context_window(&self) -> Option<u32> {
        self.inner.catalog_context_window()
    }

    fn config_context_window(&self) -> Option<u32> {
        self.inner.config_context_window()
    }

    fn config_max_output_tokens(&self) -> Option<u32> {
        self.inner.config_max_output_tokens()
    }

    async fn probe_context_window(&self) -> Option<u32> {
        self.inner.probe_context_window().await
    }

    fn input_modalities(&self) -> Vec<sven_model::catalog::InputModality> {
        self.inner.input_modalities()
    }

    fn input_modalities_reported(&self) -> bool {
        self.inner.input_modalities_reported()
    }

    fn image_limits(&self) -> sven_model::catalog::ImageLimits {
        self.inner.image_limits()
    }
}

/// Settles a completion with the budget when its stream is dropped, however
/// it ended.
struct Meter {
    budget: Arc<RunBudget>,
    admission: Admission,
    reported: Option<u64>,
    /// The stream reached its end (done, or a provider error) before the
    /// consumer dropped it.
    ended: bool,
    /// Sampled token ids the provider returned.
    sampled: u64,
    /// Characters streamed as reasoning, text and tool calls.
    streamed_chars: u64,
}

/// Characters a token stands for when a cut-off completion returned no ids:
/// the same rough estimate the live streaming counters use.
const CHARS_PER_TOKEN: u64 = 4;

impl Meter {
    fn observe(&mut self, item: &Result<ResponseEvent>) {
        let chars = |text: &str| u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        match item {
            Ok(ResponseEvent::Usage { output_tokens, .. }) => {
                let reported = self.reported.get_or_insert(0);
                *reported = reported.saturating_add(u64::from(*output_tokens));
            }
            Ok(ResponseEvent::Done | ResponseEvent::Error(_)) | Err(_) => self.ended = true,
            Ok(ResponseEvent::Tokens { ids, .. }) => {
                self.sampled = self.sampled.saturating_add(ids.len() as u64);
            }
            Ok(ResponseEvent::ThinkingDelta(text) | ResponseEvent::TextDelta(text)) => {
                self.streamed_chars = self.streamed_chars.saturating_add(chars(text));
            }
            Ok(ResponseEvent::ToolCall {
                name, arguments, ..
            }) => {
                self.streamed_chars = self
                    .streamed_chars
                    .saturating_add(chars(name).saturating_add(chars(arguments)));
            }
            Ok(_) => {}
        }
    }

    fn charge(&self) -> Charge {
        match (self.reported, self.ended) {
            (Some(tokens), _) => Charge::Reported(tokens),
            (None, true) => Charge::Unmetered,
            (None, false) if self.sampled > 0 => Charge::CutOff(self.sampled),
            (None, false) => Charge::CutOff(self.streamed_chars / CHARS_PER_TOKEN),
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.budget.settle(self.admission, self.charge());
    }
}

#[cfg(test)]
#[path = "run_budget_tests.rs"]
mod tests;
