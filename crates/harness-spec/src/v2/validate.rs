// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Authoring-level validation of a workflow document.
//!
//! These checks run before lowering so that a mistake is reported in the
//! author's terms (this task, that gate) rather than as a violation of the
//! generated steps. The lowered spec still goes through the shared validator
//! afterwards; nothing here replaces it.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use serde_json::Value;

use super::spec::{ArtifactCheck, Check, GateDef, WorkflowSpec};
use crate::error::HarnessError;
use crate::spec::{IOType, IterateSource, MAX_GATE_TIMEOUT_SECS, RESERVED_TOOLS};
use crate::validate::is_workspace_relative;

/// The most repair attempts one task may earn. The attempts are unrolled
/// into the plan, so this bounds its size.
pub(super) const MAX_REPAIR_ATTEMPTS: u64 = 16;

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// A name that may appear inside a generated step id.
pub(super) fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && !name.contains("__")
}

/// Splits `<gate>.<metric>`.
pub(super) fn split_metric(reference: &str) -> Option<(&str, &str)> {
    reference.split_once('.')
}

/// Validates `w` in full.
///
/// # Errors
///
/// [`HarnessError::Invalid`] naming the first problem and where it is.
pub(super) fn validate(w: &WorkflowSpec) -> Result<(), HarnessError> {
    if !is_identifier(&w.name) {
        return Err(invalid(format!(
            "workflow name `{}` must be lowercase letters, digits, `-` and `_`, start with a \
             letter, have no `__` and be at most 48 characters",
            w.name
        )));
    }
    if w.workflow.is_empty() {
        return Err(invalid("`workflow` needs at least one task".into()));
    }
    validate_execution(w)?;
    validate_agent_tools(w)?;
    validate_decoding(
        w,
        "agent",
        &super::spec::decoding_of(&w.agent.sampling, &w.agent.thinking, &w.agent.turn),
    )?;
    validate_roles(w)?;
    validate_gates(w)?;
    validate_tasks(w)?;
    super::imports::validate_imports(w)?;
    super::vars::validate(w)?;
    validate_success(w)?;
    validate_use(w)?;
    validate_capabilities(w)
}

fn validate_execution(w: &WorkflowSpec) -> Result<(), HarnessError> {
    if w.execution.resume == Some(false) {
        return Err(invalid(
            "execution.resume: false is not supported: every harness run is checkpointable and \
             resumable"
                .into(),
        ));
    }
    let limits = w.execution.limits.clone().unwrap_or_default();
    if limits.model_turns.is_none() {
        return Err(invalid(
            "execution.limits.model_turns is required: it bounds the model turns of the whole \
             run and of each agent loop (for example `limits: { model_turns: 200 }`)"
                .into(),
        ));
    }
    validate_allowance(w, "execution.limits.tool_calls", &limits.tool_calls)?;
    for (name, value) in [
        ("steps", limits.steps),
        ("model_turns", limits.model_turns),
        (
            "tool_calls",
            limits.tool_calls.as_ref().and_then(Value::as_u64),
        ),
        ("delegate_depth", limits.delegate_depth),
    ] {
        if value == Some(0) {
            return Err(invalid(format!(
                "execution.limits.{name} must be at least 1"
            )));
        }
    }
    Ok(())
}

fn validate_agent_tools(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let lists = std::iter::once(("agent.tools".to_string(), w.agent.tools.as_ref())).chain(
        w.all_tasks()
            .into_iter()
            .map(|t| (format!("task `{}`: tools", t.name), t.tools.as_ref())),
    );
    for (place, list) in lists {
        if let Some(tool) = list
            .into_iter()
            .flatten()
            .find(|tool| RESERVED_TOOLS.contains(&tool.as_str()))
        {
            return Err(invalid(format!(
                "{place}: `{tool}` is reserved to the harness's own steps and cannot be an agent tool"
            )));
        }
    }
    Ok(())
}

/// Checks an artifact check. `templated` is true inside a `for_each` task,
/// where the path may name the item (`reports/{{ item_id }}.md`); it is still
/// confined to the workspace when the verifier resolves it.
fn validate_artifact(place: &str, a: &ArtifactCheck, templated: bool) -> Result<(), HarnessError> {
    let holes = a.artifact.contains("{{");
    if !is_workspace_relative(&a.artifact) || (holes && !templated) {
        return Err(invalid(format!(
            "{place}: artifact `{}` must be a literal path inside the workspace (relative, no \
             `..`); only a `for_each` task's paths may name the item",
            a.artifact
        )));
    }
    if let Some(hash) = &a.sha256 {
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid(format!(
                "{place}: sha256 must be 64 hexadecimal characters"
            )));
        }
    }
    Ok(())
}

fn validate_gates(w: &WorkflowSpec) -> Result<(), HarnessError> {
    for (name, gate) in &w.gates {
        let place = format!("gate `{name}`");
        if !is_identifier(name) {
            return Err(invalid(format!(
                "{place}: gate names are lowercase letters, digits, `-` and `_` (no `__`)"
            )));
        }
        match gate {
            GateDef::Artifact(a) => validate_artifact(&place, a, false)?,
            GateDef::Command(c) => {
                if c.command.first().is_none_or(|p| p.trim().is_empty()) {
                    return Err(invalid(format!(
                        "{place}: `command` is an argv list whose first element names the program"
                    )));
                }
                if let Some(t) = c.timeout {
                    if t == 0 || t > MAX_GATE_TIMEOUT_SECS {
                        return Err(invalid(format!(
                            "{place}: timeout {t} s is outside 1..={MAX_GATE_TIMEOUT_SECS}"
                        )));
                    }
                }
                if c.output_bytes
                    .is_some_and(|n| !(1024..=1_048_576).contains(&n))
                {
                    return Err(invalid(format!(
                        "{place}: output_bytes must be within 1024..=1048576"
                    )));
                }
                if let Some(cwd) = &c.cwd {
                    if !is_workspace_relative(cwd) {
                        return Err(invalid(format!(
                            "{place}: cwd `{cwd}` must be inside the workspace (relative, no `..`)"
                        )));
                    }
                }
                if let Some(input) = &c.cwd_input {
                    let declared = w.inputs.as_ref().and_then(|i| i.get(input));
                    if c.cwd.is_some() {
                        return Err(invalid(format!(
                            "{place}: `cwd` and `cwd_input` are exclusive"
                        )));
                    }
                    if declared.is_none_or(|d| d.ty != IOType::String) {
                        return Err(invalid(format!(
                            "{place}: cwd_input `{input}` must name a declared input of type string"
                        )));
                    }
                }
                for (metric, decl) in &c.metrics {
                    if !is_identifier(metric) {
                        return Err(invalid(format!(
                            "{place}: metric name `{metric}` is not a plain name"
                        )));
                    }
                    if !is_workspace_relative(&decl.file) {
                        return Err(invalid(format!(
                            "{place}: metric `{metric}` file `{}` must be inside the workspace",
                            decl.file
                        )));
                    }
                    if !decl.pointer.is_empty() && !decl.pointer.starts_with('/') {
                        return Err(invalid(format!(
                            "{place}: metric `{metric}` pointer `{}` must be an RFC 6901 pointer \
                             such as /line_pct",
                            decl.pointer
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Checks that a `<gate>.<metric>` reference names a declared metric of a
/// command gate.
pub(super) fn check_metric_ref(
    w: &WorkflowSpec,
    place: &str,
    reference: &str,
) -> Result<(), HarnessError> {
    let (gate, metric) = split_metric(reference).ok_or_else(|| {
        invalid(format!(
            "{place}: metric `{reference}` is not `<gate>.<metric>`"
        ))
    })?;
    match w.gates.get(gate) {
        Some(GateDef::Command(c)) if c.metrics.contains_key(metric) => Ok(()),
        Some(GateDef::Command(c)) => Err(invalid(format!(
            "{place}: gate `{gate}` declares no metric `{metric}` (declared: {})",
            if c.metrics.is_empty() {
                "none".to_string()
            } else {
                c.metrics.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        ))),
        Some(GateDef::Artifact(_)) => Err(invalid(format!(
            "{place}: gate `{gate}` is an artifact check and has no metrics"
        ))),
        None => Err(unknown_gate(w, place, gate)),
    }
}

fn unknown_gate(w: &WorkflowSpec, place: &str, gate: &str) -> HarnessError {
    invalid(format!(
        "{place}: references gate `{gate}`, which is not declared (declared: {})",
        if w.gates.is_empty() {
            "none".to_string()
        } else {
            w.gates.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    ))
}

fn validate_tasks(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let mut seen = BTreeSet::new();
    for task in w.all_tasks() {
        let place = format!("task `{}`", task.name);
        if !is_identifier(&task.name) {
            return Err(invalid(format!(
                "{place}: task names are lowercase letters, digits, `-` and `_`, start with a \
                 letter and have no `__`"
            )));
        }
        if !seen.insert(task.name.clone()) {
            return Err(invalid(format!(
                "{place}: the name is used by more than one task"
            )));
        }
        let in_item = w.loop_of(task).for_each.is_some();
        super::body::validate_shape(w, task, &place)?;
        for out in &task.outputs {
            if !is_workspace_relative(out) || (out.contains("{{") && !in_item) {
                return Err(invalid(format!(
                    "{place}: output `{out}` must be a literal path inside the workspace; only a \
                     `for_each` task's paths may name the item"
                )));
            }
        }
        for (i, check) in task.verify.iter().enumerate() {
            let place = format!("{place}: verify[{i}]");
            match check {
                Check::Gate(name) | Check::Soft(name) if !w.gates.contains_key(name) => {
                    return Err(unknown_gate(w, &place, name));
                }
                Check::Soft(name) if !matches!(w.gates.get(name), Some(GateDef::Command(_))) => {
                    return Err(invalid(format!(
                        "{place}: only a command gate can be recorded without being required"
                    )));
                }
                Check::Gate(_) | Check::Soft(_) => {}
                Check::Artifact(a) => validate_artifact(&place, a, in_item)?,
                Check::Metric(m) => check_metric_ref(w, &place, &m.metric)?,
            }
        }
        super::body::validate_loop(w, task, &place)?;
        validate_decoding(
            w,
            &place,
            &super::spec::decoding_of(&task.sampling, &task.thinking, &task.turn),
        )?;
        validate_model_only(task, &place)?;
        validate_allowance(w, &place, &task.allowance)?;
        validate_corrections(w, task, &place)?;
        for (j, turn) in task.follow_ups.iter().enumerate() {
            let place = format!("{place}: follow_ups[{j}]");
            validate_allowance(w, &place, &turn.allowance)?;
            super::counts::validate_follow_up(w.inputs.as_ref(), turn, &place)?;
        }
        if let Some(repair) = &task.repair {
            if task.verify.is_empty() && task.outputs.is_empty() {
                return Err(invalid(format!(
                    "{place}: `repair` has nothing to repair; add `verify` or `outputs`"
                )));
            }
            super::counts::validate_repair(w.inputs.as_ref(), repair, &place)?;
        }
    }
    Ok(())
}

/// Each decoding setting is a literal within its range or a single hole over
/// the inputs, so that a typo is an error here and not a provider's refusal in
/// the middle of a run.
fn validate_decoding(
    w: &WorkflowSpec,
    place: &str,
    decoding: &crate::spec::StepDecoding,
) -> Result<(), HarnessError> {
    for (name, setting) in decoding.settings() {
        let Some(value) = setting else { continue };
        if let Value::String(text) = value {
            let template = crate::expr::Template::parse(text)?;
            let hole = template.single_hole().ok_or_else(|| {
                invalid(format!(
                    "{place}: {name} is a number or a single `{{{{ inputs.<name> }}}}`"
                ))
            })?;
            let declared = hole.root() == "inputs"
                && hole
                    .segments()
                    .get(1)
                    .is_some_and(|n| w.inputs.as_ref().is_some_and(|i| i.contains_key(n)));
            if !declared {
                return Err(invalid(format!(
                    "{place}: {name} reads `{hole}`, which is not a declared input"
                )));
            }
            continue;
        }
        let ok = match (name, value) {
            ("temperature", v) => v.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)),
            ("top_p", v) => v.as_f64().is_some_and(|p| p > 0.0 && p <= 1.0),
            (
                "top_k"
                | "thinking_budget"
                | "max_output_tokens"
                | "first_response_timeout"
                | "deadline",
                v,
            ) => v.as_u64().is_some_and(|n| n >= 1),
            ("seed", v) => v.as_u64().is_some(),
            ("thinking", v) => v.is_boolean(),
            _ => false,
        };
        if !ok {
            return Err(invalid(format!(
                "{place}: {name} `{value}` is out of range (temperature 0 to 2, top_p above 0 \
                 and at most 1, counts and timeouts at least 1, seed a non-negative integer, \
                 thinking true or false)"
            )));
        }
    }
    Ok(())
}

/// A model role is a plain name; an endpoint or a key is the host's business
/// and never appears in a document.
fn validate_roles(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let named = std::iter::once(("agent.model".to_string(), w.agent.model.as_ref()))
        .chain(
            w.all_tasks()
                .into_iter()
                .map(|t| (format!("task `{}`: model", t.name), t.model.as_ref())),
        )
        .chain(w.all_tasks().into_iter().filter_map(|t| {
            t.corrections.as_ref().map(|c| {
                (
                    format!("task `{}`: corrections.author.model", t.name),
                    c.author.model.as_ref(),
                )
            })
        }));
    for (place, role) in named {
        if let Some(role) = role {
            if !is_identifier(role) {
                return Err(invalid(format!(
                    "{place}: `{role}` is not a role name (lowercase letters, digits, `-`, `_`); \
                     endpoints and keys belong to the host, not the document"
                )));
            }
        }
    }
    Ok(())
}

/// A `corrections` block names two command gates, a distinct author and
/// bounded counts.
fn validate_corrections(
    w: &WorkflowSpec,
    task: &super::spec::TaskSpec,
    place: &str,
) -> Result<(), HarnessError> {
    let Some(c) = &task.corrections else {
        return Ok(());
    };
    let place = format!("{place}: corrections");
    if task.model_only {
        return Err(invalid(format!(
            "{place}: a `model_only` task has no agent to correct"
        )));
    }
    if c.max == 0 || c.max > 16 {
        return Err(invalid(format!("{place}: max must be within 1..=16")));
    }
    if c.retries.is_some_and(|r| r > 8) {
        return Err(invalid(format!("{place}: retries must be within 0..=8")));
    }
    for (what, name) in [("check", &c.check), ("accept", &c.accept)] {
        match w.gates.get(name) {
            Some(GateDef::Command(_)) => {}
            Some(_) => {
                return Err(invalid(format!(
                    "{place}: {what} gate `{name}` must be a command gate"
                )))
            }
            None => return Err(unknown_gate(w, &place, name)),
        }
    }
    if !is_identifier(&c.author.name) {
        return Err(invalid(format!(
            "{place}: author name `{}` is not a plain name",
            c.author.name
        )));
    }
    if w.all_tasks().iter().any(|t| t.name == c.author.name) {
        return Err(invalid(format!(
            "{place}: author name `{}` is also a task's name",
            c.author.name
        )));
    }
    validate_allowance(w, &place, &c.count)?;
    validate_allowance(w, &place, &c.allowance)?;
    validate_decoding(
        w,
        &format!("{place}: author"),
        &super::spec::decoding_of(&c.author.sampling, &c.author.thinking, &c.author.turn),
    )
}

/// A follow-up's `max` and an allowance are whole numbers or a single input.
fn validate_allowance(
    w: &WorkflowSpec,
    place: &str,
    value: &Option<Value>,
) -> Result<(), HarnessError> {
    let Some(value) = value else { return Ok(()) };
    match value {
        Value::Number(n) if n.as_u64().is_some() => Ok(()),
        Value::String(text) => {
            let template = crate::expr::Template::parse(text)?;
            let declared = template.single_hole().is_some_and(|hole| {
                hole.root() == "inputs"
                    && hole
                        .segments()
                        .get(1)
                        .is_some_and(|n| w.inputs.as_ref().is_some_and(|i| i.contains_key(n)))
            });
            if declared {
                Ok(())
            } else {
                Err(invalid(format!(
                    "{place}: allowance `{text}` is a whole number or a single declared \
                     `{{{{ inputs.<name> }}}}`"
                )))
            }
        }
        other => Err(invalid(format!(
            "{place}: allowance `{other}` is not a whole number"
        ))),
    }
}

/// A `model_only` task is one model turn without tools.
fn validate_model_only(task: &super::spec::TaskSpec, place: &str) -> Result<(), HarnessError> {
    if task.schema.is_some() && !task.model_only {
        return Err(invalid(format!(
            "{place}: `schema` goes with `model_only: true`"
        )));
    }
    if task.system.is_some() && !task.model_only {
        return Err(invalid(format!(
            "{place}: `system` belongs to a `model_only` task's own conversation; an agent task \
             shares the conversation `agent.system` starts"
        )));
    }
    if !task.model_only {
        return Ok(());
    }
    if task.tools.is_some() || !task.follow_ups.is_empty() {
        return Err(invalid(format!(
            "{place}: a `model_only` task is one model turn without tools: it takes neither \
             `tools` nor `follow_ups`"
        )));
    }
    if task.allowance.is_some() || task.max_repeated_results.is_some() {
        return Err(invalid(format!(
            "{place}: a `model_only` task makes no tool calls: no `allowance` or `max_repeated_results`"
        )));
    }
    if task.schema.as_ref().is_some_and(|s| !s.is_object()) {
        return Err(invalid(format!(
            "{place}: `schema` must be a JSON Schema object"
        )));
    }
    Ok(())
}

fn validate_success(w: &WorkflowSpec) -> Result<(), HarnessError> {
    if w.success
        .work_items
        .as_ref()
        .is_some_and(|i| i.all_completed || i.all_settled)
        && !w.workflow.iter().any(|t| t.for_each.is_some())
    {
        return Err(invalid(
            "success.work_items needs a task with `for_each`".into(),
        ));
    }
    for task in &w.workflow {
        let gates = task
            .for_each
            .iter()
            .filter_map(|e| e.on_item_failure.as_ref())
            .flat_map(|f| &f.verify);
        for gate in gates {
            if !w.gates.contains_key(gate) {
                return Err(unknown_gate(
                    w,
                    &format!("task `{}`: for_each.on_item_failure", task.name),
                    gate,
                ));
            }
        }
    }
    for name in &w.success.gates {
        if !w.gates.contains_key(name) {
            return Err(unknown_gate(w, "success.gates", name));
        }
    }
    for (reference, keys) in &w.success.metrics {
        let place = format!("success.metrics.{reference}");
        check_metric_ref(w, &place, reference)?;
        keys.threshold()
            .map_err(|e| invalid(format!("{place}: {e}")))?;
    }
    Ok(())
}

/// Every declared gate must be used: an unused gate is a check the author
/// believes runs and does not.
fn validate_use(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let mut used: BTreeSet<&str> = w.success.gates.iter().map(String::as_str).collect();
    used.extend(
        w.success
            .metrics
            .keys()
            .filter_map(|r| split_metric(r).map(|(g, _)| g)),
    );
    for task in w.all_tasks() {
        if let Some(failure) = task
            .for_each
            .as_ref()
            .and_then(|e| e.on_item_failure.as_ref())
        {
            used.extend(failure.verify.iter().map(String::as_str));
        }
        if let Some(c) = &task.corrections {
            used.insert(&c.check);
            used.insert(&c.accept);
        }
        if let Some(until) = &task.until {
            used.extend(split_metric(&until.metric).map(|(g, _)| g));
        }
        for check in &task.verify {
            match check {
                Check::Gate(name) | Check::Soft(name) => {
                    used.insert(name);
                }
                Check::Metric(m) => {
                    used.extend(split_metric(&m.metric).map(|(g, _)| g));
                }
                Check::Artifact(_) => {}
            }
        }
    }
    match w.gates.keys().find(|g| !used.contains(g.as_str())) {
        Some(gate) => Err(invalid(format!(
            "gate `{gate}` is declared but no task `verify` or `success` entry uses it"
        ))),
        None => Ok(()),
    }
}

/// The capabilities the workflow exercises: its agent profile, a shell for
/// every command gate that is used, and the verifier for every file or
/// metric check.
pub(super) fn needed_capabilities(w: &WorkflowSpec) -> BTreeSet<String> {
    let mut needed: BTreeSet<String> = w
        .agent
        .profile
        .capabilities()
        .iter()
        .map(|c| (*c).to_string())
        .collect();
    needed.extend(super::imports::imported_capabilities(w));
    let mut reads_files = false;
    let mut note = |gate: Option<&GateDef>| match gate {
        Some(GateDef::Command(_)) => {
            needed.insert("ExecuteShell".to_string());
        }
        _ => {
            needed.insert("RunVerifier".to_string());
        }
    };
    for task in w.all_tasks() {
        if !task.outputs.is_empty() {
            note(None);
        }
        if let Some(c) = &task.corrections {
            note(w.gates.get(&c.check));
            note(w.gates.get(&c.accept));
        }
        for gate in task
            .for_each
            .iter()
            .filter_map(|e| e.on_item_failure.as_ref())
            .flat_map(|f| &f.verify)
        {
            note(w.gates.get(gate));
        }
        if let Some(until) = &task.until {
            note(split_metric(&until.metric).and_then(|(g, _)| w.gates.get(g)));
            note(None);
        }
        reads_files |= task.for_each.as_ref().is_some_and(|each| {
            matches!(each.source, IterateSource::Json(_)) || !each.covers.is_empty()
        });
        for check in &task.verify {
            match check {
                Check::Gate(name) | Check::Soft(name) => note(w.gates.get(name)),
                Check::Artifact(_) => note(None),
                Check::Metric(m) => {
                    note(split_metric(&m.metric).and_then(|(g, _)| w.gates.get(g)));
                    note(None);
                }
            }
        }
    }
    for name in &w.success.gates {
        note(w.gates.get(name));
    }
    for reference in w.success.metrics.keys() {
        note(split_metric(reference).and_then(|(g, _)| w.gates.get(g)));
        note(None);
    }
    if reads_files {
        needed.insert("ReadFile".to_string());
    }
    needed
}

fn validate_capabilities(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let Some(declared) = &w.capabilities else {
        return Ok(());
    };
    let declared: BTreeSet<&str> = declared.iter().map(|c| c.as_str()).collect();
    let missing: Vec<String> = needed_capabilities(w)
        .into_iter()
        .filter(|c| !declared.contains(c.as_str()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(invalid(format!(
        "capabilities do not cover what the workflow does: missing {} (agent profile `{}` \
         needs {}; command gates need ExecuteShell; file and metric checks need RunVerifier). \
         Add them to `capabilities`, or remove `capabilities` to have them derived.",
        missing.join(", "),
        w.agent.profile.as_str(),
        if w.agent.profile.capabilities().is_empty() {
            "none".to_string()
        } else {
            w.agent.profile.capabilities().join(", ")
        }
    )))
}
