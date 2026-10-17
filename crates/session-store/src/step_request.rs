// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent cost accounting for its clients. If
// your team needs expertise in measuring what agent runs cost then you can
// procure our services by sending an email to info@swedishembedded.com.

//! Model requests as ATIF metrics: what each step read, wrote and cost, who
//! answered it, and the whole trajectory's totals.
//!
//! A step can cover several requests (tool results fold into the same
//! turn-shaped step as the next reply), so a step's figures are the sums over
//! its requests. Prompt tokens include cached ones, as ATIF defines them.
//! The model that answered goes on the step as `model_name`; the sampling
//! seed in effect for the step's first request that reported one goes in the
//! step metrics' `extra.sven.seed`. The sampled token ids and log-probabilities
//! of the step's requests are concatenated in request order.
//!
//! This is the plain account; the token ledger ([`crate::step_ledger`]) is
//! the breakdown of where the input went. Record a request with one or the
//! other, not both, or the prompt tokens are counted twice.

use atif::{FinalMetrics, StepMetrics};
use serde_json::{json, Value};
use sven_vocab::RequestRecord;

/// Key of the object in a step's metrics `extra` that holds sven's own
/// figures.
const EXTRA_KEY: &str = "sven";

/// Adds one request's account to a step's `metrics` and `model_name`.
pub fn record_response(
    metrics: &mut Option<StepMetrics>,
    model_name: &mut Option<String>,
    request: &RequestRecord,
) {
    let metrics = metrics.get_or_insert_with(StepMetrics::default);
    let add = |slot: &mut Option<u64>, n: u32| *slot = Some(slot.unwrap_or(0) + u64::from(n));
    let reported = request.reported;
    add(&mut metrics.prompt_tokens, reported.logical_prompt());
    add(&mut metrics.completion_tokens, reported.output);
    add(&mut metrics.cached_tokens, reported.cache_read);
    if let Some(cost) = request.cost_usd {
        metrics.cost_usd = Some(metrics.cost_usd.unwrap_or(0.0) + cost);
    }
    let sampled = &request.sampled;
    if !sampled.token_ids.is_empty() {
        metrics
            .completion_token_ids
            .get_or_insert_with(Vec::new)
            .extend(&sampled.token_ids);
    }
    if !sampled.logprobs.is_empty() {
        metrics
            .logprobs
            .get_or_insert_with(Vec::new)
            .extend(&sampled.logprobs);
    }
    if model_name.is_none() {
        model_name.clone_from(&sampled.model);
    }
    if let Some(seed) = sampled.seed {
        let extra = metrics.extra.get_or_insert_with(|| json!({}));
        if let Some(object) = extra.as_object_mut() {
            let sven = object.entry(EXTRA_KEY).or_insert_with(|| json!({}));
            if let Some(sven) = sven.as_object_mut() {
                sven.entry("seed").or_insert_with(|| Value::from(seed));
            }
        }
    }
}

/// The totals over `requests`, or `None` when there are none.
#[must_use]
pub fn totals<'a>(requests: impl IntoIterator<Item = &'a RequestRecord>) -> Option<FinalMetrics> {
    let mut totals = FinalMetrics::default();
    let mut any = false;
    for request in requests {
        any = true;
        let add = |slot: &mut Option<u64>, n: u32| *slot = Some(slot.unwrap_or(0) + u64::from(n));
        add(
            &mut totals.total_prompt_tokens,
            request.reported.logical_prompt(),
        );
        add(&mut totals.total_completion_tokens, request.reported.output);
        add(&mut totals.total_cached_tokens, request.reported.cache_read);
        if let Some(cost) = request.cost_usd {
            totals.total_cost_usd = Some(totals.total_cost_usd.unwrap_or(0.0) + cost);
        }
    }
    any.then_some(totals)
}
