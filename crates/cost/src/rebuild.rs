// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Whether rewriting the prompt pays for the cache it discards.
//!
//! Demoting a result, compacting the history or rebuilding a resumed session
//! all shrink the prompt, and all change it from some token on. The provider's
//! cache holds the prompt as a prefix, so everything behind the first changed
//! token is lost and the next request writes the new text again. Rebuilding
//! therefore saves `tokens_saved` input tokens on every later request but costs
//! one cache write up front; whether it pays depends on how many requests
//! remain.
//!
//! Only the prompt side is compared. What the two options share (the tokens a
//! step appends, the output) cancels.

use serde::{Deserialize, Serialize};
use sven_model_catalog::Pricing;
use sven_vocab::cost::{GateAction, GateReason, RebuildKind, RebuildRecord, Recommendation};
use sven_vocab::Amount;

use crate::cache::CacheState;
use crate::model::{estimate, Placement, PromptShape, Recovery, StepInputs};

/// A model call the rebuild itself makes (the summary of a compaction), priced
/// as a cold request: its prompt matches nothing in the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generation {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A rewrite of the prompt being considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildCandidate {
    pub kind: RebuildKind,
    /// Prompt size before the rebuild.
    pub prompt_tokens: u32,
    /// Tokens the rebuild removes from the prompt.
    pub tokens_saved: u32,
    /// Tokens before the first one the rebuild changes.
    pub first_changed_token: u32,
    pub generation: Generation,
}

/// The comparison of keeping the prompt with rebuilding it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Assessment {
    pub recommendation: Recommendation,
    pub keep: Amount,
    pub rebuild: Amount,
    /// `keep - rebuild`.
    pub saving: Amount,
    pub break_even_requests: Option<f64>,
}

/// Prompt-side cost of `requests` requests of a prompt of `prefix_tokens`,
/// the first starting from `warm` cached tokens and the rest from a prefix the
/// first left fully cached. Returns the first request's cost and the cost of
/// each later one.
fn per_request(pricing: Option<&Pricing>, prefix_tokens: u32, warm: u32) -> (Amount, Amount) {
    let prompt = |warm_tokens| {
        estimate(
            pricing,
            &StepInputs {
                shape: PromptShape {
                    prefix_tokens,
                    new_tokens: 0,
                    output_tokens: 0,
                },
                placement: Placement::Continue(CacheState { warm_tokens }),
                recovery: Recovery::none(),
            },
        )
        .total()
    };
    (prompt(warm), prompt(prefix_tokens))
}

/// Weigh `candidate` over `remaining_requests` (at least one) given how much of
/// the prompt `cache` is expected to hold.
#[must_use]
pub fn assess(
    pricing: Option<&Pricing>,
    cache: CacheState,
    candidate: &RebuildCandidate,
    remaining_requests: u32,
) -> Assessment {
    let later = f64::from(remaining_requests.max(1) - 1);
    let rebuilt_tokens = candidate
        .prompt_tokens
        .saturating_sub(candidate.tokens_saved);
    // The part of the cache in front of the first changed token survives.
    let surviving = cache.warm_tokens.min(candidate.first_changed_token);

    let (keep_first, keep_later) = per_request(pricing, candidate.prompt_tokens, cache.warm_tokens);
    let (rebuild_first, rebuild_later) = per_request(pricing, rebuilt_tokens, surviving);
    let call = estimate(
        pricing,
        &StepInputs {
            shape: PromptShape {
                prefix_tokens: 0,
                new_tokens: candidate.generation.input_tokens,
                output_tokens: candidate.generation.output_tokens,
            },
            placement: Placement::Switch,
            recovery: Recovery::none(),
        },
    )
    .total();

    let keep = keep_first.plus(keep_later.times(later));
    let rebuild = call.plus(rebuild_first).plus(rebuild_later.times(later));
    let saving = keep.minus(rebuild);

    let recommendation = match saving {
        Amount::Unknown => Recommendation::Unknown,
        Amount::Known(s) if s > 0.0 => Recommendation::Rebuild,
        Amount::Known(_) => Recommendation::Keep,
    };
    Assessment {
        recommendation,
        keep,
        rebuild,
        saving,
        break_even_requests: break_even(
            keep_first.minus(call.plus(rebuild_first)),
            keep_later.minus(rebuild_later),
        ),
    }
}

/// The request count at which `first + (n - 1) * each_later` reaches zero, at
/// least one; `None` when it never does or an input is unknown.
fn break_even(first: Amount, each_later: Amount) -> Option<f64> {
    let (first, later) = (first.value()?, each_later.value()?);
    if first > 0.0 {
        return Some(1.0);
    }
    (later > 0.0).then(|| 1.0 - first / later)
}

/// How many requests to weigh a rebuild over: the declared figure, else about
/// as many as the session has made so far (a session that has lasted `k`
/// requests is expected to last about `k` more), at least one and at most `cap`.
#[must_use]
pub fn expected_remaining_requests(requests_so_far: u32, declared: Option<u32>, cap: u32) -> u32 {
    declared.unwrap_or_else(|| requests_so_far.clamp(1, cap.max(1)))
}

/// What the gate does with a rebuild.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheAware {
    /// Nothing is weighed and nothing is recorded.
    #[default]
    Off,
    /// The comparison is recorded; the rebuild goes ahead regardless.
    Advise,
    /// A discretionary rebuild that does not break even is skipped.
    Enforce,
}

/// Whether a rebuild can be left undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Necessity {
    /// The prompt would not fit the model's window without it.
    RequiredToFit,
    /// An optimisation.
    Discretionary,
}

/// The gate's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateDecision {
    pub action: GateAction,
    pub reason: GateReason,
}

/// Decide what to do with a rebuild. Only `Enforce` ever skips, only a
/// discretionary rebuild is skipped, and only on a known comparison that
/// favours keeping: the window and an undeclared price both outrank cost.
#[must_use]
pub fn gate(
    mode: CacheAware,
    necessity: Necessity,
    recommendation: Recommendation,
) -> GateDecision {
    let (action, reason) = match (mode, necessity, recommendation) {
        (CacheAware::Off, _, _) => (GateAction::Rebuilt, GateReason::NotEvaluated),
        (_, Necessity::RequiredToFit, _) => (GateAction::Rebuilt, GateReason::RequiredToFit),
        (CacheAware::Advise, _, _) => (GateAction::Rebuilt, GateReason::Advisory),
        (CacheAware::Enforce, _, Recommendation::Keep) => {
            (GateAction::Skipped, GateReason::DoesNotBreakEven)
        }
        (CacheAware::Enforce, _, Recommendation::Rebuild) => {
            (GateAction::Rebuilt, GateReason::BreaksEven)
        }
        (CacheAware::Enforce, _, Recommendation::Unknown) => {
            (GateAction::Rebuilt, GateReason::PriceUnknown)
        }
    };
    GateDecision { action, reason }
}

/// The ledger's record of a decision.
#[must_use]
pub fn record(
    candidate: &RebuildCandidate,
    assessment: &Assessment,
    decision: &GateDecision,
    cache: CacheState,
    remaining_requests: u32,
) -> RebuildRecord {
    RebuildRecord {
        kind: candidate.kind,
        recommendation: assessment.recommendation,
        action: decision.action,
        reason: decision.reason,
        prompt_tokens: candidate.prompt_tokens,
        tokens_saved: candidate.tokens_saved,
        first_changed_token: candidate.first_changed_token,
        warm_tokens: cache.warm_tokens,
        remaining_requests,
        keep: assessment.keep,
        rebuild: assessment.rebuild,
        saving: assessment.saving,
        break_even_requests: assessment.break_even_requests,
    }
}
