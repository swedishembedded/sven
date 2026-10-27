// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! What the cache-aware gate needs from the executor: the price of the model,
//! what its cache is expected to hold, and how long the session has run.

use std::collections::BTreeMap;
use sven_rt::Instant;

use sven_cost::{CacheAware, CacheState};
use sven_model_catalog::{lookup_pricing, Pricing};

use super::TurnExecutor;
use crate::CacheContext;

/// How the executor weighs a rewrite of the prompt against the provider's
/// cache (`cost.cache_aware` and its neighbours).
#[derive(Debug, Clone, Default)]
pub struct CostPolicy {
    /// `off` weighs nothing, `advise` records the comparison, `enforce` also
    /// leaves a demotion undone that would not pay.
    pub mode: CacheAware,
    /// Requests still expected in the session, when declared; otherwise about
    /// as many as it has made so far.
    pub declared_remaining: Option<u32>,
    /// Declared prices by `provider/model`, taking precedence over the
    /// catalog's.
    pub prices: BTreeMap<String, Pricing>,
}

/// What the previous requests left in the provider's cache.
#[derive(Debug, Default)]
pub(super) struct CacheUsage {
    /// When the last request finished, and what it read and wrote.
    last: Option<(Instant, u32)>,
    /// Requests the session has made.
    requests: u32,
}

impl CacheUsage {
    /// Notes a finished request that read and wrote `cached` prompt tokens.
    pub(super) fn finished(&mut self, cached: u32) {
        self.last = Some((Instant::now(), cached));
        self.requests = self.requests.saturating_add(1);
    }
}

impl TurnExecutor {
    /// Weigh demotions against the prompt cache as `policy` says.
    #[must_use]
    pub fn with_cost_policy(mut self, policy: CostPolicy) -> Self {
        self.cost = policy;
        self
    }

    /// The gate's view of the request about to be built, or `None` when it
    /// is off.
    pub(super) fn cache_context(
        &self,
        model: &dyn sven_model::ModelProvider,
    ) -> Option<CacheContext> {
        if self.cost.mode == CacheAware::Off {
            return None;
        }
        let pricing = self
            .cost
            .prices
            .get(&format!("{}/{}", model.name(), model.model_name()))
            .copied()
            .or_else(|| lookup_pricing(model.name(), model.model_name()));
        let ttl = pricing.and_then(|p| p.cache).map(|c| c.ttl_secs);
        let usage = self
            .cache_usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (idle, cached) = usage.last.map_or((None, 0), |(at, cached)| {
            (Some(at.elapsed().as_secs()), cached)
        });
        let warm = CacheState::expected(cached, ttl, idle).warm_tokens;
        let window = model.catalog_context_window();
        let output = self.limits.cap_output(model.catalog_max_output_tokens());
        Some(CacheContext {
            mode: self.cost.mode,
            pricing,
            warm_tokens: warm,
            requests_so_far: usage.requests,
            declared_remaining: self.cost.declared_remaining,
            input_budget: sven_model::budget::effective_input_budget(window, output),
        })
    }
}
