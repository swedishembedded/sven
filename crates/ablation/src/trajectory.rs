// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a saved trajectory says about decisions.
//!
//! Only the fields the report needs are read, from the JSON as sven writes
//! it: per step, the ledger records under `metrics.extra.sven_ledger` (each
//! may carry the decision cost since the previous request) and the verdicts
//! under `metrics.extra.sven_decisions`.

use serde_json::Value;

use crate::record::DecisionRecord;

const LEDGER_KEY: &str = "sven_ledger";
const DECISIONS_KEY: &str = "sven_decisions";

fn extra<'a>(step: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    step.pointer(&format!("/metrics/extra/{key}"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The last message the agent sent in `trajectory`: the answer to the final
/// question.
#[must_use]
pub fn final_reply(trajectory: &Value) -> Option<String> {
    trajectory
        .get("steps")?
        .as_array()?
        .iter()
        .rev()
        .find(|s| s.get("source").and_then(Value::as_str) == Some("agent"))?
        .get("message")?
        .as_str()
        .map(str::to_owned)
}

/// The decision account of one trajectory: the inference calls and the time
/// waited on them, and every verdict as recorded. Sub-agent trajectories are
/// not folded in.
#[must_use]
pub fn decision_record(trajectory: &Value) -> DecisionRecord {
    let mut record = DecisionRecord::default();
    let steps = trajectory
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for step in steps {
        for request in extra(step, LEDGER_KEY) {
            record.inference_calls += request
                .pointer("/decision/calls")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            record.latency_ms += request
                .pointer("/decision/latency_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
        }
        for verdict in extra(step, DECISIONS_KEY) {
            match verdict.get("verdict").and_then(Value::as_str) {
                Some("accept") => record.accepted += 1,
                Some("abstain") => record.abstained += 1,
                _ => {}
            }
            if verdict.get("applied").and_then(Value::as_bool) == Some(true) {
                record.applied += 1;
            }
            record.traces.push(verdict.clone());
        }
    }
    record
}
