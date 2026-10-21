// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Counts the run decides: how many repair turns, how many times a follow-up.
//!
//! These are unrolled into steps, so the plan needs a bound it can unroll; the
//! run then chooses how much of that bound to use. A count is a literal, or a
//! single `{{ inputs.<name> }}` (a number input) together with `at_most`, the
//! bound the plan is unrolled to. Each unrolled unit past the first is guarded:
//! it happens only when the input is at least its number.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::expr::Template;
use crate::steps::{IOType, InputValue};
use crate::workflow::{FollowUp, Repair};
use crate::workflow_validate::MAX_REPAIR_ATTEMPTS;

/// A count as the document gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Count {
    /// How many units the plan has (the literal, or `at_most`).
    pub unroll: u64,
    /// The number input that says how many of them a run uses.
    pub input: Option<String>,
}

/// Reads a count from `value` (a whole number, or a single input hole) and
/// the `at_most` beside it.
///
/// # Errors
///
/// A count outside `range`, a hole that is not a declared number input, an
/// input hole without `at_most`, or `at_most` beside a literal.
pub(super) fn parse(
    value: &Value,
    at_most: Option<u64>,
    range: &RangeInclusive<u64>,
    inputs: Option<&BTreeMap<String, InputValue>>,
    place: &str,
) -> Result<Count, HarnessError> {
    let bad = |why: String| HarnessError::Invalid(format!("{place}: {why}"));
    let (lo, hi) = (*range.start(), *range.end());
    match value {
        Value::Number(n) => {
            let count = n.as_u64().filter(|c| range.contains(c));
            if at_most.is_some() {
                return Err(bad("`at_most` goes with a count read from an input".into()));
            }
            count
                .map(|unroll| Count {
                    unroll,
                    input: None,
                })
                .ok_or_else(|| bad(format!("{n} is outside {lo}..={hi}")))
        }
        Value::String(text) => {
            let hole = Template::parse(text)
                .ok()
                .and_then(|t| t.single_hole().cloned())
                .ok_or_else(|| {
                    bad(format!(
                        "`{text}` is neither a whole number nor one `{{{{ inputs.<name> }}}}`"
                    ))
                })?;
            let segments = hole.segments();
            let name = match segments {
                [root, name] if root == "inputs" => name.clone(),
                _ => {
                    return Err(bad(format!(
                        "`{text}` must read one input, `{{{{ inputs.<name> }}}}`"
                    )))
                }
            };
            if inputs
                .and_then(|i| i.get(&name))
                .is_none_or(|i| i.ty != IOType::Number)
            {
                return Err(bad(format!(
                    "input `{name}` must be declared with type number"
                )));
            }
            let unroll = at_most
                .filter(|m| range.contains(m))
                .ok_or_else(|| bad(format!("a count read from an input needs `at_most` within {lo}..={hi}, the most the plan allows")))?;
            Ok(Count {
                unroll,
                input: Some(name),
            })
        }
        other => Err(bad(format!(
            "{other} is not a whole number or an input hole"
        ))),
    }
}

/// The condition "input `name` is at least `k`": it is not any of 0..k.
pub(super) fn at_least(name: &str, k: u64) -> Value {
    let below: Vec<Value> = (0..k)
        .map(|j| json!({ "eq": format!("inputs.{name}"), "value": j }))
        .collect();
    json!({ "not": { "any": below } })
}

/// A follow-up's `max` and `at_most`, checked.
pub(super) fn validate_follow_up(
    inputs: Option<&BTreeMap<String, InputValue>>,
    turn: &FollowUp,
    place: &str,
) -> Result<(), HarnessError> {
    match &turn.max {
        Some(max) => parse(
            max,
            turn.at_most,
            &(1..=8),
            inputs,
            &format!("{place}: max"),
        )
        .map(drop),
        None if turn.at_most.is_some() => Err(HarnessError::Invalid(format!(
            "{place}: `at_most` goes with a `max` read from an input"
        ))),
        None => Ok(()),
    }
}

/// A repair's `attempts` and `at_most`, checked.
pub(super) fn validate_repair(
    inputs: Option<&BTreeMap<String, InputValue>>,
    repair: &Repair,
    place: &str,
) -> Result<(), HarnessError> {
    parse(
        &repair.attempts,
        repair.at_most,
        &(1..=MAX_REPAIR_ATTEMPTS),
        inputs,
        &format!("{place}: repair.attempts"),
    )
    .map(drop)
}
