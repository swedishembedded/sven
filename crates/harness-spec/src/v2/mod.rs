// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The declarative workflow language (`api_version: "2"`, ADR 0005).
//!
//! A workflow document is decoded into a typed AST ([`spec`]), checked with
//! errors written for its author ([`validate`]) and lowered ([`lower`]) into
//! the same [`HarnessSpec`] a version 1 document parses into, which then goes
//! through the shared static validator and plan compiler. There is no second
//! interpreter: the machine runs the lowered steps.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod body;
mod checks;
mod context;
mod corrections;
mod counts;
mod imports;
mod lower;
mod rewrite;
mod scratchpad;
pub mod spec;
mod system;
mod validate;
mod vars;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::HarnessError;
use crate::spec::HarnessSpec;
use spec::{
    ArtifactCheck, Check, CommandGate, GateDef, MetricCheck, SuccessSpec, TaskSpec, WorkflowSpec,
};

const TOP_LEVEL_KEYS: [&str; 14] = [
    "variables",
    "imports",
    "on_failure",
    "api_version",
    "name",
    "description",
    "inputs",
    "outputs",
    "agent",
    "execution",
    "capabilities",
    "gates",
    "workflow",
    "success",
];

/// Decodes `section` as `T`, naming where in the document it was.
fn decode<T: DeserializeOwned>(section: Value, where_: &str) -> Result<T, HarnessError> {
    serde_json::from_value(section).map_err(|e| HarnessError::Parse(format!("{where_}: {e}")))
}

/// Decodes a workflow document from its raw value.
///
/// # Errors
///
/// [`HarnessError::Parse`] for an unknown key or a wrongly shaped section,
/// naming the section.
pub fn parse_workflow(value: Value) -> Result<WorkflowSpec, HarnessError> {
    let Value::Object(mut map) = value else {
        return Err(HarnessError::Parse(
            "a workflow document is a map".to_string(),
        ));
    };
    if let Some(unknown) = map.keys().find(|k| !TOP_LEVEL_KEYS.contains(&k.as_str())) {
        return Err(HarnessError::Parse(format!(
            "unknown top-level field `{unknown}` (known: {})",
            TOP_LEVEL_KEYS.join(", ")
        )));
    }
    let mut take = |key: &str| map.remove(key);
    let name = match take("name") {
        Some(Value::String(name)) => name,
        _ => {
            return Err(HarnessError::Parse(
                "`name` is required and a string".into(),
            ))
        }
    };
    let description = take("description")
        .map(|v| decode::<String>(v, "description"))
        .transpose()?;
    let inputs = take("inputs").map(|v| decode(v, "inputs")).transpose()?;
    let outputs = take("outputs").map(|v| decode(v, "outputs")).transpose()?;
    let agent = take("agent")
        .map(|v| decode(v, "agent"))
        .transpose()?
        .unwrap_or_default();
    let execution = take("execution")
        .map(|v| decode(v, "execution"))
        .transpose()?
        .unwrap_or_default();
    let capabilities = take("capabilities")
        .map(|v| decode(v, "capabilities"))
        .transpose()?;
    let gates = parse_gates(take("gates"))?;
    let workflow = parse_workflow_tasks(take("workflow"))?;
    let success = take("success")
        .map(|v| decode::<SuccessSpec>(v, "success"))
        .transpose()?
        .unwrap_or_default();
    let on_failure = take("on_failure")
        .map(|v| decode(v, "on_failure"))
        .transpose()?
        .unwrap_or_default();
    let variables = take("variables")
        .map(|v| decode(v, "variables"))
        .transpose()?
        .unwrap_or_default();
    let imports = take("imports")
        .map(|v| decode(v, "imports"))
        .transpose()?
        .unwrap_or_default();
    Ok(WorkflowSpec {
        name,
        description,
        inputs,
        outputs,
        agent,
        execution,
        capabilities,
        gates,
        workflow,
        success,
        on_failure,
        imports,
        variables,
        resolved: std::collections::BTreeMap::new(),
    })
}

fn parse_gates(
    section: Option<Value>,
) -> Result<std::collections::BTreeMap<String, GateDef>, HarnessError> {
    let mut gates = std::collections::BTreeMap::new();
    let Some(section) = section else {
        return Ok(gates);
    };
    let Value::Object(map) = section else {
        return Err(HarnessError::Parse("`gates` is a map of gates".into()));
    };
    for (name, raw) in map {
        let where_ = format!("gates.{name}");
        let kind = raw
            .as_object()
            .map(|o| (o.contains_key("command"), o.contains_key("artifact")));
        let gate = match kind {
            Some((true, false)) => GateDef::Command(decode::<CommandGate>(raw, &where_)?),
            Some((false, true)) => GateDef::Artifact(decode::<ArtifactCheck>(raw, &where_)?),
            _ => {
                return Err(HarnessError::Parse(format!(
                    "{where_}: a gate has exactly one of `command` (run an argv) or `artifact` \
                     (a file must exist)"
                )))
            }
        };
        gates.insert(name, gate);
    }
    Ok(gates)
}

fn parse_check(raw: Value, where_: &str) -> Result<Check, HarnessError> {
    match raw {
        Value::String(name) => Ok(Check::Gate(name)),
        Value::Object(mut map) if map.contains_key("artifact") => Ok(Check::Artifact(decode(
            Value::Object(std::mem::take(&mut map)),
            where_,
        )?)),
        Value::Object(map) if map.contains_key("gate") => {
            let (Some(Value::String(name)), Some(Value::Bool(false)), 2) = (
                map.get("gate").cloned(),
                map.get("required").cloned(),
                map.len(),
            ) else {
                return Err(HarnessError::Parse(format!(
                    "{where_}: write `{{gate: <name>, required: false}}` for a check whose result \
                     is recorded and never fails the task"
                )));
            };
            Ok(Check::Soft(name))
        }
        Value::Object(mut map) if map.contains_key("metric") => {
            let metric = match map.remove("metric") {
                Some(Value::String(metric)) => metric,
                _ => return Err(HarnessError::Parse(format!("{where_}: `metric` is a string"))),
            };
            let keys: spec::ThresholdKeys = decode(Value::Object(map), where_)?;
            let threshold = keys
                .threshold()
                .map_err(|e| HarnessError::Parse(format!("{where_}: metric `{metric}`: {e}")))?;
            Ok(Check::Metric(MetricCheck { metric, threshold }))
        }
        _ => Err(HarnessError::Parse(format!(
            "{where_}: a check is a gate name, `{{artifact: path}}` or `{{metric: gate.name, gte: N}}`"
        ))),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    task: String,
    #[serde(default, rename = "use")]
    use_import: Option<String>,
    #[serde(default)]
    with: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    when: Option<Value>,
    #[serde(default)]
    follow_ups: Vec<spec::FollowUp>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    verify: Vec<Value>,
    #[serde(default)]
    repair: Option<spec::Repair>,
    #[serde(default)]
    for_each: Option<spec::ForEach>,
    #[serde(default)]
    until: Option<RawUntil>,
    #[serde(default)]
    model_only: bool,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default)]
    sampling: Option<spec::Sampling>,
    #[serde(default)]
    thinking: Option<spec::Thinking>,
    #[serde(default)]
    turn: Option<spec::TurnBounds>,
    #[serde(default)]
    allowance: Option<Value>,
    #[serde(default)]
    max_repeated_results: Option<u32>,
    #[serde(default)]
    corrections: Option<spec::Corrections>,
    #[serde(default)]
    checkpoint: Option<spec::CheckpointPolicy>,
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default, rename = "do")]
    body: Vec<Value>,
    #[serde(default)]
    vars: std::collections::BTreeMap<String, spec::VarUse>,
}

/// `until:`; the comparison keys are collected and checked by
/// [`spec::ThresholdKeys`], which rejects unknown ones (serde cannot combine
/// `flatten` with `deny_unknown_fields`).
#[derive(serde::Deserialize)]
struct RawUntil {
    metric: String,
    max_iterations: u64,
    #[serde(flatten)]
    threshold: std::collections::BTreeMap<String, Value>,
}

fn parse_workflow_tasks(section: Option<Value>) -> Result<Vec<TaskSpec>, HarnessError> {
    let Some(Value::Array(items)) = section else {
        return Err(HarnessError::Parse(
            "`workflow` is required and is a list of tasks".into(),
        ));
    };
    parse_tasks(items, "workflow")
}

fn parse_tasks(items: Vec<Value>, list: &str) -> Result<Vec<TaskSpec>, HarnessError> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, raw)| {
            let label = raw
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("{list}[{index}]"), |n| format!("task `{n}`"));
            parse_task(raw, &label)
        })
        .collect()
}

fn parse_task(raw: Value, label: &str) -> Result<TaskSpec, HarnessError> {
    let raw: RawTask = decode(raw, label)?;
    let verify = raw
        .verify
        .into_iter()
        .enumerate()
        .map(|(i, check)| parse_check(check, &format!("{label}: verify[{i}]")))
        .collect::<Result<_, _>>()?;
    let until = raw
        .until
        .map(|u| {
            let keys: spec::ThresholdKeys =
                decode(Value::Object(u.threshold.into_iter().collect()), label)?;
            let threshold = keys
                .threshold()
                .map_err(|e| HarnessError::Parse(format!("{label}: until: {e}")))?;
            Ok::<_, HarnessError>(spec::Until {
                metric: u.metric,
                threshold,
                max_iterations: u.max_iterations,
            })
        })
        .transpose()?;
    let body = parse_tasks(raw.body, &format!("{label}: do"))?;
    Ok(TaskSpec {
        name: raw.name,
        description: raw.description,
        task: raw.task,
        tools: raw.tools,
        when: raw.when,
        follow_ups: raw.follow_ups,
        outputs: raw.outputs,
        verify,
        repair: raw.repair,
        for_each: raw.for_each,
        until,
        model_only: raw.model_only,
        system: raw.system,
        model: raw.model,
        schema: raw.schema,
        sampling: raw.sampling,
        thinking: raw.thinking,
        turn: raw.turn,
        allowance: raw.allowance,
        max_repeated_results: raw.max_repeated_results,
        corrections: raw.corrections,
        checkpoint: raw.checkpoint,
        use_import: raw.use_import,
        with: raw.with,
        conversation: raw.conversation,
        vars: raw.vars,
        body,
    })
}

/// Decodes, validates and lowers a workflow document into the v1 spec the
/// shared validator and plan compiler take. `resolved` holds the interface of
/// each import the package resolved, by alias.
///
/// # Errors
///
/// A decoding error, the first authoring-level validation failure, an import
/// that is not in `resolved`, or a `use` that does not fit the interface of
/// what it uses.
pub fn lower_document_with(
    value: Value,
    resolved: std::collections::BTreeMap<String, spec::Imported>,
) -> Result<HarnessSpec, HarnessError> {
    let mut workflow = parse_workflow(value)?;
    workflow.resolved = resolved;
    validate::validate(&workflow)?;
    lower::lower(&workflow)
}

/// The `imports` of a raw workflow document, alias to path as written.
///
/// # Errors
///
/// [`HarnessError::Parse`] when `imports` is not a map of strings.
pub fn imports_of(
    value: &Value,
) -> Result<std::collections::BTreeMap<String, String>, HarnessError> {
    match value.get("imports") {
        None => Ok(std::collections::BTreeMap::new()),
        Some(section) => decode(section.clone(), "imports"),
    }
}
