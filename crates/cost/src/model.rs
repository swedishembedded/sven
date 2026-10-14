// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Pricing one step of a conversation.

use sven_model_catalog::Pricing;

use crate::cache::CacheState;
use sven_vocab::Amount;

const PER_MTOK: f64 = 1_000_000.0;

/// How many tokens a step sends and receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptShape {
    /// Tokens an earlier request already sent, in the same order.
    pub prefix_tokens: u32,
    /// Tokens this step appends to the prefix.
    pub new_tokens: u32,
    /// Tokens the model is expected to generate.
    pub output_tokens: u32,
}

/// Where the step runs relative to the request before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The same model as the previous request, with this much of the prefix
    /// still in its provider's cache.
    Continue(CacheState),
    /// A different model: its cache holds none of this conversation, so the
    /// whole prefix is sent again.
    Switch,
}

/// The expected extra cost of the step failing and being redone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recovery {
    /// Declared probability, in `[0, 1]`, that the step must be redone.
    pub failure_probability: f64,
    /// What redoing it costs.
    pub rerun: Amount,
}

impl Recovery {
    /// The step is not expected to be redone.
    #[must_use]
    pub fn none() -> Self {
        Self {
            failure_probability: 0.0,
            rerun: Amount::ZERO,
        }
    }
}

/// Everything that determines the cost of one step apart from prices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepInputs {
    pub shape: PromptShape,
    pub placement: Placement,
    pub recovery: Recovery,
}

/// The cost of one step, term by term. The terms do not overlap: their sum is
/// the cost, and no token is charged in two of them.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostTerms {
    /// New tokens, and the whole prompt when the provider has no cache.
    pub input_uncached: Amount,
    /// Prefix tokens served from the cache.
    pub cached_read: Amount,
    /// Prefix tokens the cache no longer held, written again.
    pub cache_write: Amount,
    /// Generated tokens.
    pub output: Amount,
    /// The prefix sent afresh to a different model.
    pub handoff: Amount,
    /// Probability-weighted cost of redoing the step.
    pub recovery: Amount,
}

impl CostTerms {
    fn all(amount: Amount) -> Self {
        Self {
            input_uncached: amount,
            cached_read: amount,
            cache_write: amount,
            output: amount,
            handoff: amount,
            recovery: amount,
        }
    }

    /// The sum of the terms; unknown when any term is.
    #[must_use]
    pub fn total(&self) -> Amount {
        [
            self.input_uncached,
            self.cached_read,
            self.cache_write,
            self.output,
            self.handoff,
        ]
        .into_iter()
        .fold(self.recovery, Amount::plus)
    }
}

fn tokens(count: u32, per_mtok: f64) -> Amount {
    Amount::Known(f64::from(count) * per_mtok / PER_MTOK)
}

/// Price one step. `None` pricing is an undeclared price: every term is
/// unknown, including the ones that would have been zero.
#[must_use]
pub fn estimate(pricing: Option<&Pricing>, inputs: &StepInputs) -> CostTerms {
    let Some(pricing) = pricing else {
        return CostTerms::all(Amount::Unknown);
    };
    let PromptShape {
        prefix_tokens,
        new_tokens,
        output_tokens,
    } = inputs.shape;
    let recovery = if inputs.recovery.failure_probability == 0.0 {
        Amount::ZERO
    } else {
        inputs
            .recovery
            .rerun
            .times(inputs.recovery.failure_probability)
    };
    let mut terms = CostTerms {
        input_uncached: tokens(new_tokens, pricing.input_per_mtok),
        cached_read: Amount::ZERO,
        cache_write: Amount::ZERO,
        output: tokens(output_tokens, pricing.output_per_mtok),
        handoff: Amount::ZERO,
        recovery,
    };
    let Some(cache) = pricing.cache else {
        terms.input_uncached = tokens(
            prefix_tokens.saturating_add(new_tokens),
            pricing.input_per_mtok,
        );
        return terms;
    };
    match inputs.placement {
        Placement::Continue(state) => {
            let warm = state.warm_tokens.min(prefix_tokens);
            terms.cached_read = tokens(warm, cache.read_per_mtok);
            terms.cache_write = tokens(prefix_tokens - warm, cache.write_per_mtok);
        }
        Placement::Switch => terms.handoff = tokens(prefix_tokens, cache.write_per_mtok),
    }
    terms
}
