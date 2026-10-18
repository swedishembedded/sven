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
    // A step that already holds a request is about to hold two: keep each
    // request's own prompt, tokens and identity under `extra.sven.requests`,
    // because the step's own fields are then the first request's prompt and
    // every request's tokens run together. The first request's facts are, at
    // this moment, exactly the step's fields.
    if metrics.prompt_tokens.is_some() {
        keep_each_request(metrics, model_name.as_deref(), request);
    }
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
    if metrics.prompt_token_ids.is_none() && !sampled.prompt_token_ids.is_empty() {
        metrics.prompt_token_ids = Some(sampled.prompt_token_ids.clone());
    }
    if model_name.is_none() {
        model_name.clone_from(&sampled.model);
    }
    // What identifies the decode goes under `extra.sven`; the first request of
    // the step that reported a field fixes it.
    let identity = &sampled.identity;
    let facts: [(&str, Option<Value>); 5] = [
        ("seed", sampled.seed.map(Value::from)),
        (
            "adapter",
            identity
                .adapter
                .as_ref()
                .map(|a| json!({ "id": a.id, "digest": a.digest })),
        ),
        ("base_digest", identity.base_digest.clone().map(Value::from)),
        ("backend", identity.backend.clone().map(Value::from)),
        ("engine", identity.engine.clone().map(Value::from)),
    ];
    for (key, value) in facts {
        let Some(value) = value else { continue };
        let extra = metrics.extra.get_or_insert_with(|| json!({}));
        if let Some(object) = extra.as_object_mut() {
            let sven = object.entry(EXTRA_KEY).or_insert_with(|| json!({}));
            if let Some(sven) = sven.as_object_mut() {
                sven.entry(key).or_insert(value);
            }
        }
    }
}

/// One request's own record, as kept under `extra.sven.requests`.
fn request_entry(
    model: Option<&str>,
    seed: Option<u64>,
    prompt_token_ids: Option<&[i64]>,
    completion_token_ids: Option<&[i64]>,
    logprobs: Option<&[f64]>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) -> Value {
    let mut entry = serde_json::Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            entry.insert(key.into(), value);
        }
    };
    put("model", model.map(Value::from));
    put("seed", seed.map(Value::from));
    put("prompt_tokens", prompt_tokens.map(Value::from));
    put("completion_tokens", completion_tokens.map(Value::from));
    put("prompt_token_ids", prompt_token_ids.map(|v| json!(v)));
    put(
        "completion_token_ids",
        completion_token_ids.map(|v| json!(v)),
    );
    put("logprobs", logprobs.map(|v| json!(v)));
    Value::Object(entry)
}

/// Adds `request` to `extra.sven.requests`, first creating the list from the
/// request the step already holds.
fn keep_each_request(metrics: &mut StepMetrics, model: Option<&str>, request: &RequestRecord) {
    let sven_seed = metrics
        .extra
        .as_ref()
        .and_then(|e| e.get(EXTRA_KEY))
        .and_then(|s| s.get("seed"))
        .and_then(Value::as_u64);
    let first = request_entry(
        model,
        sven_seed,
        metrics.prompt_token_ids.as_deref(),
        metrics.completion_token_ids.as_deref(),
        metrics.logprobs.as_deref(),
        metrics.prompt_tokens,
        metrics.completion_tokens,
    );
    let sampled = &request.sampled;
    let this = request_entry(
        sampled.model.as_deref(),
        sampled.seed,
        Some(&sampled.prompt_token_ids)
            .filter(|v| !v.is_empty())
            .map(Vec::as_slice),
        Some(&sampled.token_ids)
            .filter(|v| !v.is_empty())
            .map(Vec::as_slice),
        Some(&sampled.logprobs)
            .filter(|v| !v.is_empty())
            .map(Vec::as_slice),
        Some(u64::from(request.reported.logical_prompt())),
        Some(u64::from(request.reported.output)),
    );
    let extra = metrics.extra.get_or_insert_with(|| json!({}));
    let Some(object) = extra.as_object_mut() else {
        return;
    };
    let sven = object.entry(EXTRA_KEY).or_insert_with(|| json!({}));
    let Some(sven) = sven.as_object_mut() else {
        return;
    };
    match sven.get_mut("requests").and_then(Value::as_array_mut) {
        Some(list) => list.push(this),
        None => {
            sven.insert("requests".into(), json!([first, this]));
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
