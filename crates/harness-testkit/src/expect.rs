// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Expectation checking and JSON diffs.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};

use crate::case::Expect;
use crate::report::{JsonDiff, Mismatch, RunStatus, RunSummary};
use crate::run::FixtureFacts;

/// Every position where `expected` and `actual` differ.
///
/// Objects are compared key by key and arrays index by index; a value whose
/// kind changed (object to array, scalar to object) is one difference at its
/// own position. Positions are RFC 6901 pointers; the root is `""`.
#[must_use]
pub fn json_diff(expected: &Value, actual: &Value) -> Vec<JsonDiff> {
    let mut diffs = Vec::new();
    diff_at("", expected, actual, false, &mut diffs);
    diffs
}

/// Every position where `actual` lacks or contradicts `expected`.
///
/// Objects in `expected` are a subset test (extra keys in `actual` are
/// fine, recursively); arrays and scalars must be equal.
#[must_use]
pub fn includes(expected: &Value, actual: &Value) -> Vec<JsonDiff> {
    let mut diffs = Vec::new();
    diff_at("", expected, actual, true, &mut diffs);
    diffs
}

fn diff_at(pointer: &str, expected: &Value, actual: &Value, subset: bool, out: &mut Vec<JsonDiff>) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(have)) => {
            let mut keys: Vec<&String> = want.keys().collect();
            if !subset {
                keys.extend(have.keys().filter(|k| !want.contains_key(*k)));
            }
            keys.sort();
            for key in keys {
                let child = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (want.get(key), have.get(key)) {
                    (Some(w), Some(h)) => diff_at(&child, w, h, subset, out),
                    (w, h) => out.push(leaf(child, w, h)),
                }
            }
        }
        (Value::Array(want), Value::Array(have)) => {
            for index in 0..want.len().max(have.len()) {
                let child = format!("{pointer}/{index}");
                match (want.get(index), have.get(index)) {
                    // Arrays are compared exactly even inside a subset test.
                    (Some(w), Some(h)) => diff_at(&child, w, h, false, out),
                    (w, h) => out.push(leaf(child, w, h)),
                }
            }
        }
        (w, h) if w != h => out.push(leaf(pointer.to_string(), Some(w), Some(h))),
        _ => {}
    }
}

fn leaf(pointer: String, expected: Option<&Value>, actual: Option<&Value>) -> JsonDiff {
    JsonDiff {
        pointer,
        expected: expected.cloned(),
        actual: actual.cloned(),
    }
}

fn mismatch(
    prefix: &str,
    name: &str,
    expected: Value,
    actual: Value,
    diff: Vec<JsonDiff>,
) -> Mismatch {
    Mismatch {
        expectation: format!("{prefix}{name}"),
        expected,
        actual,
        diff,
    }
}

fn status_word(status: RunStatus) -> Value {
    serde_json::to_value(status).unwrap_or(Value::Null)
}

/// The expectations in `expect` that the run did not meet. `prefix` names
/// the phase (`""` or `"resume."`) in each mismatch.
pub(crate) fn check(
    expect: &Expect,
    summary: &RunSummary,
    facts: &FixtureFacts,
    prefix: &str,
) -> Vec<Mismatch> {
    let mut found = Vec::new();
    let mut add = |name: &str, expected: Value, actual: Value, diff: Vec<JsonDiff>| {
        found.push(mismatch(prefix, name, expected, actual, diff));
    };
    if summary.status != expect.status {
        let failure = summary
            .failure
            .as_ref()
            .map_or(Value::Null, |f| json!(f.code));
        add(
            "status",
            status_word(expect.status),
            json!({"status": status_word(summary.status), "failure": failure}),
            Vec::new(),
        );
    }
    let output = summary.output.clone().unwrap_or(Value::Null);
    if let Some(want) = &expect.output {
        if summary.output.as_ref() != Some(want) {
            add(
                "output",
                want.clone(),
                output.clone(),
                json_diff(want, &output),
            );
        }
    }
    if let Some(want) = &expect.output_includes {
        let diff = includes(want, &output);
        if !diff.is_empty() {
            add("output_includes", want.clone(), output.clone(), diff);
        }
    }
    if let Some(want) = &expect.failure {
        let got = summary.failure.as_ref();
        if let Some(code) = &want.code {
            if got.map(|f| &f.code) != Some(code) {
                add(
                    "failure.code",
                    json!(code),
                    got.map_or(Value::Null, |f| json!(f.code)),
                    Vec::new(),
                );
            }
        }
        if let Some(part) = &want.message_contains {
            if !got.is_some_and(|f| f.message.contains(part.as_str())) {
                add(
                    "failure.message_contains",
                    json!(part),
                    got.map_or(Value::Null, |f| json!(f.message)),
                    Vec::new(),
                );
            }
        }
    }
    for (step, want) in &expect.steps {
        let got = summary.step_counts.get(step).copied().unwrap_or(0);
        if got != *want {
            add(
                &format!("steps.{step}"),
                json!(want),
                json!(got),
                Vec::new(),
            );
        }
    }
    if let Some(max) = expect.max_steps {
        if summary.steps > max {
            add("max_steps", json!(max), json!(summary.steps), Vec::new());
        }
    }
    if let Some(want) = &expect.tools_invoked {
        let mut sorted = want.clone();
        sorted.sort();
        if sorted != summary.tools_invoked {
            add(
                "tools_invoked",
                json!(sorted),
                json!(summary.tools_invoked),
                Vec::new(),
            );
        }
    }
    let ran: Vec<&String> = expect
        .tools_not_invoked
        .iter()
        .filter(|t| summary.tools_invoked.contains(t))
        .collect();
    if !ran.is_empty() {
        add(
            "tools_not_invoked",
            json!(expect.tools_not_invoked),
            json!(ran),
            Vec::new(),
        );
    }
    let exercised: Vec<String> = expect
        .forbidden_capabilities
        .iter()
        .filter(|c| facts.capabilities.contains(c))
        .map(|c| format!("{c:?}"))
        .collect();
    if !exercised.is_empty() {
        add(
            "forbidden_capabilities",
            json!(expect
                .forbidden_capabilities
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()),
            json!(exercised),
            Vec::new(),
        );
    }
    if let Some(max) = expect.max_output_tokens {
        let got = summary.output_tokens.unwrap_or(0);
        if got > max {
            add("max_output_tokens", json!(max), json!(got), Vec::new());
        }
    }
    found
}
