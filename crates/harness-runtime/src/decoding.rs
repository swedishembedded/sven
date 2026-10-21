// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a model turn is sent with besides its prompt: the conversation it is
//! in, the system message that opens it and its decoding settings.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::Value;
use sven_harness_spec::Template;

use crate::run::HarnessRunState;

/// A step's decoding settings with their input references resolved, as the
/// turn request carries them. A setting that does not resolve to a value of
/// its kind is an error naming it: the run fails rather than sampling with a
/// setting nobody chose.
pub(crate) fn resolve_decoding(
    run: &HarnessRunState,
    d: &sven_harness_spec::StepDecoding,
) -> Result<sven_vocab::TurnDecoding, String> {
    let resolve = |name: &str, setting: &Option<Value>| -> Result<Option<Value>, String> {
        let Some(raw) = setting else { return Ok(None) };
        match run.resolve_value(raw) {
            Value::Null => Err(format!("setting `{name}` reads an input that has no value")),
            value => Ok(Some(value)),
        }
    };
    let float = |name: &str, setting: &Option<Value>| -> Result<Option<f64>, String> {
        resolve(name, setting)?
            .map(|v| {
                v.as_f64()
                    .ok_or_else(|| format!("setting `{name}` is not a number"))
            })
            .transpose()
    };
    let count = |name: &str, setting: &Option<Value>| -> Result<Option<u64>, String> {
        resolve(name, setting)?
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| format!("setting `{name}` is not a whole number"))
            })
            .transpose()
    };
    let small = |name: &str, setting: &Option<Value>| -> Result<Option<u32>, String> {
        count(name, setting)?
            .map(|n| u32::try_from(n).map_err(|_| format!("setting `{name}` is too large")))
            .transpose()
    };
    Ok(sven_vocab::TurnDecoding {
        temperature: float("temperature", &d.temperature)?,
        top_p: float("top_p", &d.top_p)?,
        top_k: small("top_k", &d.top_k)?,
        seed: count("seed", &d.seed)?,
        thinking: resolve("thinking", &d.thinking)?
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| "setting `thinking` is not true or false".to_string())
            })
            .transpose()?,
        // An input that is empty (null, or empty text) leaves the budget
        // unset: unbounded thinking is a choice a run may make.
        thinking_budget: match d.thinking_budget.as_ref().map(|raw| run.resolve_value(raw)) {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.trim().is_empty() => None,
            Some(_) => small("thinking_budget", &d.thinking_budget)?,
        },
        max_output_tokens: small("max_output_tokens", &d.max_output_tokens)?,
        first_response_timeout: count("first_response_timeout", &d.first_response_timeout)?,
        deadline: count("deadline", &d.deadline)?,
    })
}

/// How a model turn differs from the plain one: the conversation it is in,
/// the system message that opens it and its decoding settings.
#[derive(Clone, Copy)]
pub(crate) struct TurnShape<'a> {
    pub(crate) conversation: Option<&'a str>,
    pub(crate) system: Option<&'a Template>,
    pub(crate) origin: Option<&'a str>,
    pub(crate) role: Option<&'a str>,
    pub(crate) decoding: sven_vocab::TurnDecoding,
}

/// A system message template, rendered against the run.
pub(crate) fn system_text(state: &HarnessRunState, system: Option<&Template>) -> Option<String> {
    system.map(|t| match state.resolve_template(t) {
        Value::String(text) => text,
        other => other.to_string(),
    })
}
