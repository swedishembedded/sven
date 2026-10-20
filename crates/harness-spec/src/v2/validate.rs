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

use super::spec::{ArtifactCheck, Check, GateDef, WorkflowSpec};
use crate::error::HarnessError;
use crate::spec::{GATE_TOOL, MAX_GATE_TIMEOUT_SECS};
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
    validate_gates(w)?;
    validate_tasks(w)?;
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
    for (name, value) in [
        ("steps", limits.steps),
        ("model_turns", limits.model_turns),
        ("tool_calls", limits.tool_calls),
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
        w.workflow
            .iter()
            .map(|t| (format!("task `{}`: tools", t.name), t.tools.as_ref())),
    );
    for (place, list) in lists {
        if list.into_iter().flatten().any(|tool| tool == GATE_TOOL) {
            return Err(invalid(format!(
                "{place}: `{GATE_TOOL}` is reserved to gates and cannot be an agent tool"
            )));
        }
    }
    Ok(())
}

fn validate_artifact(place: &str, a: &ArtifactCheck) -> Result<(), HarnessError> {
    if !is_workspace_relative(&a.artifact) || a.artifact.contains("{{") {
        return Err(invalid(format!(
            "{place}: artifact `{}` must be a literal path inside the workspace (relative, no `..`)",
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
            GateDef::Artifact(a) => validate_artifact(&place, a)?,
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
                if let Some(cwd) = &c.cwd {
                    if !is_workspace_relative(cwd) {
                        return Err(invalid(format!(
                            "{place}: cwd `{cwd}` must be inside the workspace (relative, no `..`)"
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
    for task in &w.workflow {
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
        if task.task.trim().is_empty() {
            return Err(invalid(format!("{place}: `task` is empty")));
        }
        for out in &task.outputs {
            if !is_workspace_relative(out) || out.contains("{{") {
                return Err(invalid(format!(
                    "{place}: output `{out}` must be a literal path inside the workspace"
                )));
            }
        }
        for (i, check) in task.verify.iter().enumerate() {
            let place = format!("{place}: verify[{i}]");
            match check {
                Check::Gate(name) if !w.gates.contains_key(name) => {
                    return Err(unknown_gate(w, &place, name));
                }
                Check::Gate(_) => {}
                Check::Artifact(a) => validate_artifact(&place, a)?,
                Check::Metric(m) => check_metric_ref(w, &place, &m.metric)?,
            }
        }
        if let Some(repair) = &task.repair {
            if task.verify.is_empty() && task.outputs.is_empty() {
                return Err(invalid(format!(
                    "{place}: `repair` has nothing to repair; add `verify` or `outputs`"
                )));
            }
            if repair.attempts == 0 || repair.attempts > MAX_REPAIR_ATTEMPTS {
                return Err(invalid(format!(
                    "{place}: repair.attempts {} is outside 1..={MAX_REPAIR_ATTEMPTS}",
                    repair.attempts
                )));
            }
        }
    }
    Ok(())
}

fn validate_success(w: &WorkflowSpec) -> Result<(), HarnessError> {
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
    for task in &w.workflow {
        for check in &task.verify {
            match check {
                Check::Gate(name) => {
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
pub(super) fn needed_capabilities(w: &WorkflowSpec) -> BTreeSet<&'static str> {
    let mut needed: BTreeSet<&'static str> =
        w.agent.profile.capabilities().iter().copied().collect();
    let mut note = |gate: Option<&GateDef>| match gate {
        Some(GateDef::Command(_)) => {
            needed.insert("ExecuteShell");
        }
        _ => {
            needed.insert("RunVerifier");
        }
    };
    for task in &w.workflow {
        if !task.outputs.is_empty() {
            note(None);
        }
        for check in &task.verify {
            match check {
                Check::Gate(name) => note(w.gates.get(name)),
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
    needed
}

fn validate_capabilities(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let Some(declared) = &w.capabilities else {
        return Ok(());
    };
    let declared: BTreeSet<&str> = declared.iter().map(|c| c.as_str()).collect();
    let missing: Vec<&str> = needed_capabilities(w)
        .into_iter()
        .filter(|c| !declared.contains(c))
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
