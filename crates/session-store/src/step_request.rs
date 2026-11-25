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
    // The sampler's top list runs parallel to the step's `logprobs`, so it is concatenated the same way.
    for (key, rows) in top_lists(sampled) {
        let extra = metrics.extra.get_or_insert_with(|| json!({}));
        let Some(object) = extra.as_object_mut() else {
            continue;
        };
        let sven = object.entry(EXTRA_KEY).or_insert_with(|| json!({}));
        let Some(sven) = sven.as_object_mut() else {
            continue;
        };
        match sven.get_mut(key).and_then(Value::as_array_mut) {
            Some(held) => held.extend(rows.as_array().cloned().unwrap_or_default()),
            None => {
                sven.insert(key.into(), rows);
            }
        }
    }
    if metrics.prompt_token_ids.is_none() && !sampled.prompt_token_ids.is_empty() {
        metrics.prompt_token_ids = Some(sampled.prompt_token_ids.clone());
    }
    if model_name.is_none() {
        model_name.clone_from(&sampled.model);
    }
    // What identifies the decode goes under `extra.sven`; the first request of
    // the step that reported a field fixes it. `requests` is authoritative
    // for a step that holds several requests.
    for (key, value) in decode_facts(sampled) {
        let extra = metrics.extra.get_or_insert_with(|| json!({}));
        if let Some(object) = extra.as_object_mut() {
            let sven = object.entry(EXTRA_KEY).or_insert_with(|| json!({}));
            if let Some(sven) = sven.as_object_mut() {
                sven.entry(key).or_insert(value);
            }
        }
    }
}

/// The sampler's top list of one request as `(key, rows)`: `generation_top_ids` and `generation_top_log_probs`, one
/// row per generated token, absent when the request did not ask for it.
fn top_lists(sampled: &sven_vocab::Sampled) -> Vec<(&'static str, Value)> {
    if sampled.top_token_ids.is_empty() {
        return Vec::new();
    }
    vec![
        ("generation_top_ids", json!(sampled.top_token_ids)),
        ("generation_top_log_probs", json!(sampled.top_logprobs)),
    ]
}

/// The keys [`decode_facts`] can produce.
const DECODE_FACT_KEYS: [&str; 15] = [
    "seed",
    "decoding",
    "sampling_honoured",
    "sampling_unhonoured",
    "adapter",
    "base_digest",
    "backend",
    "engine",
    "policy_version",
    "policy_lineage",
    "tokenizer_digest",
    "chat_template_digest",
    "logprob_kind",
    "logprob_temperature",
    "truncated",
];

/// The facts that say who decoded one request and under which settings, each
/// present only when the provider reported it. Both the step's own
/// `extra.sven` and a [`request_entry`] take exactly these, so a request's
/// record and a single-request step's cannot differ.
fn decode_facts(sampled: &sven_vocab::Sampled) -> Vec<(&'static str, Value)> {
    let identity = &sampled.identity;
    let controls = |set: sven_vocab::SamplingControls| (!set.is_empty()).then(|| json!(set));
    let facts: [(&'static str, Option<Value>); 15] = [
        ("seed", sampled.seed.map(Value::from)),
        ("decoding", sampled.decoding.as_ref().map(|d| json!(d))),
        // Which of the controls the request set took effect, and which were
        // only sent on best effort: a learner's "seeded" reads the first.
        ("sampling_honoured", controls(sampled.sampling_honoured)),
        ("sampling_unhonoured", controls(sampled.sampling_unhonoured)),
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
        (
            "policy_version",
            identity.policy_version.clone().map(Value::from),
        ),
        (
            "policy_lineage",
            identity.policy_lineage.as_ref().map(|l| json!(l)),
        ),
        (
            "tokenizer_digest",
            identity.tokenizer_digest.clone().map(Value::from),
        ),
        (
            "chat_template_digest",
            identity.chat_template_digest.clone().map(Value::from),
        ),
        ("logprob_kind", identity.logprob_kind.map(|k| json!(k))),
        (
            "logprob_temperature",
            identity.logprob_temperature.map(Value::from),
        ),
        // Only a cut-off is worth saying; a finished response is the rule.
        ("truncated", sampled.truncated.then(|| Value::from(true))),
    ];
    facts
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect()
}

/// One request's own record, as kept under `extra.sven.requests`.
fn request_entry(
    model: Option<&str>,
    prompt_token_ids: Option<&[i64]>,
    completion_token_ids: Option<&[i64]>,
    logprobs: Option<&[f64]>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    facts: Vec<(&'static str, Value)>,
) -> Value {
    let mut entry = serde_json::Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            entry.insert(key.into(), value);
        }
    };
    put("model", model.map(Value::from));
    put("prompt_tokens", prompt_tokens.map(Value::from));
    put("completion_tokens", completion_tokens.map(Value::from));
    put("prompt_token_ids", prompt_token_ids.map(|v| json!(v)));
    put(
        "completion_token_ids",
        completion_token_ids.map(|v| json!(v)),
    );
    put("logprobs", logprobs.map(|v| json!(v)));
    for (key, value) in facts {
        put(key, Some(value));
    }
    Value::Object(entry)
}

/// Adds `request` to `extra.sven.requests`, first creating the list from the
/// request the step already holds.
fn keep_each_request(metrics: &mut StepMetrics, model: Option<&str>, request: &RequestRecord) {
    // The step holds one request so far, so what `extra.sven` says is that request's.
    let held: Vec<(&'static str, Value)> = DECODE_FACT_KEYS
        .iter()
        .chain(&["generation_top_ids", "generation_top_log_probs"])
        .filter_map(|&key| {
            let value = metrics.extra.as_ref()?.get(EXTRA_KEY)?.get(key)?;
            Some((key, value.clone()))
        })
        .collect();
    let first = request_entry(
        model,
        metrics.prompt_token_ids.as_deref(),
        metrics.completion_token_ids.as_deref(),
        metrics.logprobs.as_deref(),
        metrics.prompt_tokens,
        metrics.completion_tokens,
        held,
    );
    let sampled = &request.sampled;
    let this = request_entry(
        sampled.model.as_deref(),
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
        decode_facts(sampled)
            .into_iter()
            .chain(top_lists(sampled))
            .collect(),
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

/// The oldest and newest policy version among `versions`, when every one is a
/// whole number: versions that cannot be ordered give no span, and none is
/// guessed.
#[must_use]
pub fn policy_version_span<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<(u64, u64)> {
    let mut span: Option<(u64, u64)> = None;
    for version in versions {
        let version = version.parse::<u64>().ok()?;
        span = Some(span.map_or((version, version), |(old, new)| {
            (old.min(version), new.max(version))
        }));
    }
    span
}

/// `final_metrics.extra` carrying `span` as `sven.policy_version_span`
/// (`[oldest, newest]`), or `None` when there is none.
#[must_use]
pub fn span_extra(span: Option<(u64, u64)>) -> Option<Value> {
    span.map(|(old, new)| json!({ EXTRA_KEY: { "policy_version_span": [old, new] } }))
}

/// The policy versions the requests behind `metrics` report: each entry of
/// `extra.sven.requests` when the step holds several, else the step's own.
pub fn step_policy_versions(metrics: &StepMetrics) -> Vec<&str> {
    let Some(sven) = metrics.extra.as_ref().and_then(|e| e.get(EXTRA_KEY)) else {
        return Vec::new();
    };
    fn version(entry: &Value) -> Option<&str> {
        entry.get("policy_version").and_then(Value::as_str)
    }
    match sven.get("requests").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(version).collect(),
        None => version(sven).into_iter().collect(),
    }
}

/// The totals over `requests`, or `None` when there are none.
#[must_use]
pub fn totals<'a>(requests: impl IntoIterator<Item = &'a RequestRecord>) -> Option<FinalMetrics> {
    let mut totals = FinalMetrics::default();
    let mut any = false;
    let mut versions: Vec<String> = Vec::new();
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
        if let Some(version) = &request.sampled.identity.policy_version {
            versions.push(version.clone());
        }
    }
    totals.extra = span_extra(policy_version_span(versions.iter().map(String::as_str)));
    any.then_some(totals)
}
