// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Static validation: everything that can be rejected before a plan exists.
//!
//! Validation is total over the typed spec — every step, every reference,
//! every declared contract — because a harness must fail offline, at
//! `validate` time, never halfway through a run. The checks:
//!
//! 1. `api_version` is supported, and a version 1 document uses nothing
//!    that only version 2 has.
//! 2. The entry step exists; every `then`/branch/retry/parallel reference
//!    names a step in the map (no dangling targets).
//! 3. Terminal reachability: every step can reach a terminal (`return` or
//!    `fail`) — a cycle with no exit is an unbounded cycle and is rejected.
//!    Bounded cycles (through `retry`, whose attempts bound them) are fine.
//! 4. Capabilities: primitives that exercise a capability (`tool` and
//!    `agent` -> ExecuteShell, `verify` -> RunVerifier, `delegate` and
//!    `parallel` -> SpawnChild) require the harness to declare it.
//! 5. Limits: a harness whose steps can run away (model, tool, verify,
//!    retry, parallel, delegate) must declare `limits` with the relevant
//!    bounds.
//! 6. The output contract: every `return` step's value shape satisfies the
//!    declared `outputs`.
//! 7. The schema set: every declared schema is a JSON object, every
//!    `predict` step's `schema` names a declared schema, and every declared
//!    schema is named by some step.

use crate::error::HarnessError;
use crate::expr::{Expr, Template};
use crate::spec::{
    HarnessSpec, IterateSource, StepSpec, MAX_GATE_TIMEOUT_SECS, MAX_ITEMS_LIMIT,
    MAX_ITERATIONS_LIMIT, RESERVED_TOOLS, SUPPORTED_API_VERSION, WORKFLOW_API_VERSION,
};

/// The run-data namespaces a path may root at.
const PATH_ROOTS: [&str; 4] = ["inputs", "facts", "steps", "env"];

/// Validates `spec` in full.
///
/// # Errors
///
/// [`HarnessError::Invalid`] naming the first structural problem; the
/// message always names the offending step or reference.
pub fn validate(spec: &HarnessSpec) -> Result<(), HarnessError> {
    validate_version(spec)?;
    validate_structure(spec)?;
    validate_schemas(spec)?;
    validate_references(spec)?;
    validate_entry_reaches_every_step(spec)?;
    validate_reachability(spec)?;
    validate_capabilities(spec)?;
    validate_limits(spec)?;
    validate_outputs(spec)
}

/// Validates the declared schema set: every schema is a JSON object (the
/// only shape a JSON Schema document takes), and every declared schema is
/// named by some `predict` step — a schema nothing references is dead
/// weight the digest would pin without anyone reading it.
fn validate_schemas(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let Some(schemas) = &spec.schemas else {
        return Ok(());
    };
    for (name, schema) in schemas {
        if !schema.is_object() {
            return Err(HarnessError::Invalid(format!(
                "schema `{name}` must be a JSON Schema object, got {schema}"
            )));
        }
    }
    let used: std::collections::BTreeSet<&str> = spec
        .steps
        .values()
        .filter_map(|step| match step {
            StepSpec::Predict(p) => p.schema.as_deref(),
            _ => None,
        })
        .collect();
    for name in schemas.keys() {
        if !used.contains(name.as_str()) {
            return Err(HarnessError::Invalid(format!(
                "schema `{name}` is declared but no `predict` step names it"
            )));
        }
    }
    Ok(())
}

fn validate_version(spec: &HarnessSpec) -> Result<(), HarnessError> {
    if spec.api_version == WORKFLOW_API_VERSION {
        return Ok(());
    }
    if spec.api_version != SUPPORTED_API_VERSION {
        return Err(HarnessError::Invalid(format!(
            "unsupported api_version `{}` (this compiler supports `{SUPPORTED_API_VERSION}` and \
             `{WORKFLOW_API_VERSION}`)",
            spec.api_version
        )));
    }
    for (id, step) in &spec.steps {
        let only_v2 = match step {
            StepSpec::Gate(_) | StepSpec::Iterate(_) => Some(format!("primitive `{}`", step.key())),
            StepSpec::Delegate(d) if d.propagate_failure => {
                Some("`propagate_failure` on a delegate".to_string())
            }
            _ => None,
        };
        if let Some(what) = only_v2 {
            return Err(HarnessError::Invalid(format!(
                "step `{id}` uses {what}, which needs `api_version: \"{WORKFLOW_API_VERSION}\"`"
            )));
        }
    }
    Ok(())
}

fn validate_structure(spec: &HarnessSpec) -> Result<(), HarnessError> {
    if spec.name.trim().is_empty() {
        return Err(HarnessError::Invalid(
            "harness `name` must not be empty".into(),
        ));
    }
    if spec.steps.is_empty() {
        return Err(HarnessError::Invalid(
            "harness needs at least one step".into(),
        ));
    }
    Ok(())
}

fn validate_references(spec: &HarnessSpec) -> Result<(), HarnessError> {
    if !spec.steps.contains_key(&spec.entry) {
        return Err(HarnessError::Invalid(format!(
            "entry step `{}` does not exist (steps: {})",
            spec.entry,
            spec.steps.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    for (id, step) in &spec.steps {
        for target in successors(step) {
            if !spec.steps.contains_key(&target) {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}` references unknown step `{target}`"
                )));
            }
        }
        if let StepSpec::Predict(p) = step {
            let Some(name) = &p.schema else { continue };
            if !declared_schemas(spec).any(|(declared, _)| declared == name) {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}` names schema `{name}`, which the document does not declare"
                )));
            }
        }
        validate_step_shape(id, step)?;
        validate_step_paths(id, step)?;
    }
    Ok(())
}

/// Every step must be reachable from `entry`: an orphan is dead code whose
/// capability and limit requirements would still bind the plan, and in a
/// generated document it usually marks a mistyped `then`.
fn validate_entry_reaches_every_step(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let mut reached = std::collections::BTreeSet::new();
    let mut frontier = vec![spec.entry.clone()];
    while let Some(id) = frontier.pop() {
        if !reached.insert(id.clone()) {
            continue;
        }
        if let Some(step) = spec.steps.get(&id) {
            frontier.extend(successors(step));
        }
    }
    let orphans: Vec<&str> = spec
        .steps
        .keys()
        .filter(|id| !reached.contains(*id))
        .map(String::as_str)
        .collect();
    if orphans.is_empty() {
        return Ok(());
    }
    Err(HarnessError::Invalid(format!(
        "unreachable step(s) [{}]: no path from entry `{}` leads to them",
        orphans.join(", "),
        spec.entry
    )))
}

/// The document's declared schemas: `(name, value)` pairs, sorted by name.
fn declared_schemas(spec: &HarnessSpec) -> impl Iterator<Item = (&String, &serde_json::Value)> {
    spec.schemas.iter().flatten()
}

/// Validates one step's internal shape (beyond what serde enforced).
fn validate_step_shape(id: &str, step: &StepSpec) -> Result<(), HarnessError> {
    match step {
        StepSpec::Retry(r) => {
            if r.attempts == 0 {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}`: retry attempts must be >= 1"
                )));
            }
            if r.on_exhausted.is_none() {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}`: retry needs `on_exhausted`"
                )));
            }
        }
        StepSpec::Branch(b) => {
            Expr::parse_value(&b.on).map_err(|e| {
                HarnessError::Invalid(format!("step `{id}`: bad branch condition: {e}"))
            })?;
        }
        StepSpec::Parallel(p) if p.branches.is_empty() => {
            return Err(HarnessError::Invalid(format!(
                "step `{id}`: parallel needs at least one branch"
            )));
        }
        StepSpec::Agent(a) => {
            if let Some(tool) = a
                .tools
                .iter()
                .flatten()
                .find(|t| RESERVED_TOOLS.contains(&t.as_str()))
            {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}`: `{tool}` is reserved to the harness's own steps and cannot be \
                     an agent tool"
                )));
            }
        }
        StepSpec::Gate(g) => validate_gate_shape(id, g)?,
        StepSpec::Iterate(i) => validate_iterate_shape(id, i)?,
        _ => {}
    }
    Ok(())
}

/// An `iterate` step repeats exactly one way and within a stated bound.
fn validate_iterate_shape(
    id: &str,
    iterate: &crate::spec::IterateStepSpec,
) -> Result<(), HarnessError> {
    let bad = |why: &str| HarnessError::Invalid(format!("step `{id}`: {why}"));
    match (&iterate.for_each, &iterate.until) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(bad("an iterate has exactly one of `for_each` or `until`"))
        }
        (Some(each), None) => {
            if each.max_items == 0 || each.max_items > MAX_ITEMS_LIMIT {
                return Err(bad(&format!(
                    "max_items must be within 1..={MAX_ITEMS_LIMIT}"
                )));
            }
            match &each.source {
                IterateSource::Json(json) => {
                    if !is_workspace_relative(&json.file) {
                        return Err(bad("the source file must be inside the workspace"));
                    }
                    if !json.pointer.is_empty() && !json.pointer.starts_with('/') {
                        return Err(bad("the source pointer must be an RFC 6901 pointer"));
                    }
                }
                IterateSource::Input(name) if name.is_empty() => {
                    return Err(bad("the source input needs a name"))
                }
                IterateSource::Input(_) => {}
            }
            if iterate.on_exhausted.is_some() {
                return Err(bad("`on_exhausted` belongs to `until`"));
            }
        }
        (None, Some(until)) => {
            if until.max_iterations == 0 || until.max_iterations > MAX_ITERATIONS_LIMIT {
                return Err(bad(&format!(
                    "max_iterations must be within 1..={MAX_ITERATIONS_LIMIT}"
                )));
            }
            if iterate.on_exhausted.is_none() {
                return Err(bad("an `until` needs `on_exhausted`"));
            }
        }
    }
    Ok(())
}

/// A `gate` step runs a real command, so its definition is checked tightly:
/// a non-empty argv, a bounded timeout and a working directory that stays in
/// the workspace.
fn validate_gate_shape(id: &str, gate: &crate::spec::GateStepSpec) -> Result<(), HarnessError> {
    if gate
        .command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return Err(HarnessError::Invalid(format!(
            "step `{id}`: a gate's `command` is an argv whose first element names the program"
        )));
    }
    if gate.timeout == 0 || gate.timeout > MAX_GATE_TIMEOUT_SECS {
        return Err(HarnessError::Invalid(format!(
            "step `{id}`: gate timeout {} s is outside 1..={MAX_GATE_TIMEOUT_SECS}",
            gate.timeout
        )));
    }
    if let Some(cwd) = &gate.cwd {
        if !is_workspace_relative(cwd) {
            return Err(HarnessError::Invalid(format!(
                "step `{id}`: gate cwd `{cwd}` must be a path inside the workspace (relative, no `..`)"
            )));
        }
    }
    Ok(())
}

/// `true` for a non-empty relative path with no parent-directory component.
/// A string-level check: the tools that act on the path confine it again.
pub(crate) fn is_workspace_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|part| part == "..")
}

/// Validates every path and template in the step: well-formed, rooted in a
/// known namespace.
fn validate_step_paths(id: &str, step: &StepSpec) -> Result<(), HarnessError> {
    for template_text in template_texts(step) {
        let template = Template::parse(&template_text).map_err(|e| {
            HarnessError::Invalid(format!("step `{id}`: bad template `{template_text}`: {e}"))
        })?;
        for path in template.paths() {
            check_path_root(id, path)?;
        }
    }
    if let StepSpec::Branch(b) = step {
        let expr = Expr::parse_value(&b.on)
            .map_err(|e| HarnessError::Invalid(format!("step `{id}`: {e}")))?;
        for path in expr.paths() {
            check_path_root(id, path)?;
        }
    }
    Ok(())
}

fn check_path_root(id: &str, path: &crate::expr::Path) -> Result<(), HarnessError> {
    let root = path.root();
    if PATH_ROOTS.contains(&root) {
        Ok(())
    } else {
        Err(HarnessError::Invalid(format!(
            "step `{id}`: path `{path}` roots at unknown namespace `{root}` (known: {})",
            PATH_ROOTS.join(", ")
        )))
    }
}

fn template_texts(step: &StepSpec) -> Vec<String> {
    match step {
        StepSpec::Predict(p) => vec![p.prompt.clone()],
        StepSpec::Agent(a) => vec![a.prompt.clone()],
        StepSpec::Tool(t) => template_texts_in_value(&t.args),
        StepSpec::Assign(a) => a.set.values().flat_map(&template_texts_in_value).collect(),
        StepSpec::Delegate(d) => template_texts_in_value(&d.input),
        StepSpec::Transform(t) => template_texts_in_value(&t.input),
        StepSpec::Fail(f) => f.message.clone().into_iter().collect(),
        StepSpec::Gate(g) => g.command.clone(),
        StepSpec::Verify(_)
        | StepSpec::Branch(_)
        | StepSpec::Retry(_)
        | StepSpec::Parallel(_)
        | StepSpec::Iterate(_)
        | StepSpec::Return(_) => Vec::new(),
    }
}

fn template_texts_in_value(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(items) => items.iter().flat_map(template_texts_in_value).collect(),
        serde_json::Value::Object(map) => map.values().flat_map(template_texts_in_value).collect(),
        _ => Vec::new(),
    }
}

/// Every step id a step may hand control to.
fn successors(step: &StepSpec) -> Vec<String> {
    match step {
        StepSpec::Predict(p) => vec![p.then.clone()],
        StepSpec::Agent(a) => vec![a.then.clone()],
        StepSpec::Tool(t) => vec![t.then.clone()],
        StepSpec::Verify(v) => vec![v.then.clone()],
        StepSpec::Assign(a) => vec![a.then.clone()],
        StepSpec::Branch(b) => vec![b.when_true.clone(), b.when_false.clone()],
        StepSpec::Retry(r) => {
            let mut out = vec![r.body.clone(), r.then.clone()];
            if let Some(alt) = &r.on_exhausted {
                out.push(alt.clone());
            }
            out
        }
        StepSpec::Parallel(p) => {
            let mut out = p.branches.clone();
            out.push(p.join.clone());
            out
        }
        StepSpec::Delegate(d) => vec![d.then.clone()],
        StepSpec::Transform(t) => vec![t.then.clone()],
        StepSpec::Gate(g) => vec![g.then.clone(), g.on_fail.clone()],
        StepSpec::Iterate(i) => {
            let mut out = vec![i.body.clone(), i.then.clone()];
            out.extend(i.on_exhausted.clone());
            out
        }
        // Terminals hand control to nobody.
        StepSpec::Return(_) | StepSpec::Fail(_) => Vec::new(),
    }
}

/// Every step must be able to reach a terminal. This is what rejects
/// unbounded cycles: a cycle in which no path exits to `return`/`fail` can
/// only run away. Cycles *through* a bounded `retry` are fine because the
/// retry's attempts bound them, and a loop with a branch out of it is fine
/// because the run's step budget bounds it.
///
/// The check is exact: it walks the reversed edge set outward from the
/// terminals, so a step is flagged precisely when no path from it reaches
/// one, however long the path to the terminal is (a generated workflow is a
/// chain of hundreds of steps).
fn validate_reachability(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let mut predecessors: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (id, step) in &spec.steps {
        for target in successors(step) {
            if let Some((key, _)) = spec.steps.get_key_value(&target) {
                predecessors
                    .entry(key.as_str())
                    .or_default()
                    .push(id.as_str());
            }
        }
    }
    let mut reaches_terminal: std::collections::BTreeSet<&str> = spec
        .steps
        .iter()
        .filter(|(_, step)| matches!(step, StepSpec::Return(_) | StepSpec::Fail(_)))
        .map(|(id, _)| id.as_str())
        .collect();
    let mut frontier: Vec<&str> = reaches_terminal.iter().copied().collect();
    while let Some(id) = frontier.pop() {
        for before in predecessors.get(id).into_iter().flatten() {
            if reaches_terminal.insert(before) {
                frontier.push(before);
            }
        }
    }
    let stuck: Vec<&str> = spec
        .steps
        .keys()
        .map(String::as_str)
        .filter(|id| !reaches_terminal.contains(id))
        .collect();
    if stuck.is_empty() {
        return Ok(());
    }
    Err(HarnessError::Invalid(format!(
        "unbounded cycle: step(s) [{}] can never reach a `return` or `fail` step",
        stuck.join(", ")
    )))
}

/// The capability each primitive exercises.
///
/// A tool or agent step's exercised capability is the *tool's* own bucket,
/// derived from its name by the kernel's naming conventions (`read_file` →
/// ReadFile, `shell` → ExecuteShell, an MCP name → unknowable here and left
/// to the run-time gate). The declared `capabilities` list stays the whole
/// contract: the permission policy grants exactly the declared set, so a
/// call whose bucket is undeclared is refused at the gate.
fn required_capability(step: &StepSpec) -> Option<&'static str> {
    let names: Vec<&str> = match step {
        StepSpec::Tool(t) => vec![t.name.as_str()],
        StepSpec::Agent(a) => a.tools.iter().flatten().map(|t| t.as_str()).collect(),
        StepSpec::Verify(_) => return Some("RunVerifier"),
        StepSpec::Delegate(_) | StepSpec::Parallel(_) => return Some("SpawnChild"),
        StepSpec::Gate(_) => return Some("ExecuteShell"),
        StepSpec::Iterate(i) => {
            return match i.for_each.as_ref().map(|each| &each.source) {
                Some(IterateSource::Json(_)) => Some("ReadFile"),
                _ => None,
            }
        }
        StepSpec::Predict(_)
        | StepSpec::Assign(_)
        | StepSpec::Branch(_)
        | StepSpec::Retry(_)
        | StepSpec::Transform(_)
        | StepSpec::Return(_)
        | StepSpec::Fail(_) => return None,
    };
    // Mirror the kernel's ExecuteShell naming convention (shell*, run_terminal*,
    // gdb*): a step that can run arbitrary commands must declare it.
    for name in names {
        if name.starts_with("shell") || name.starts_with("run_terminal") || name.starts_with("gdb")
        {
            return Some("ExecuteShell");
        }
    }
    None
}

fn validate_capabilities(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let declared: std::collections::BTreeSet<&str> = spec
        .capabilities
        .as_ref()
        .map(|caps| caps.iter().map(|c| c.as_str()).collect())
        .unwrap_or_default();
    for (id, step) in &spec.steps {
        if let Some(needed) = required_capability(step) {
            if !declared.contains(needed) {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}` exercises capability `{needed}` but the harness does not \
                     declare it (add `{needed}` to `capabilities`)"
                )));
            }
        }
    }
    Ok(())
}

fn validate_limits(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let runs_away = spec.steps.values().any(|step| {
        matches!(
            step,
            StepSpec::Predict(_)
                | StepSpec::Agent(_)
                | StepSpec::Tool(_)
                | StepSpec::Verify(_)
                | StepSpec::Retry(_)
                | StepSpec::Parallel(_)
                | StepSpec::Delegate(_)
                | StepSpec::Gate(_)
                | StepSpec::Iterate(_)
        )
    });
    if !runs_away {
        return Ok(());
    }
    let Some(limits) = &spec.limits else {
        return Err(HarnessError::Invalid(
            "harness has steps that can run away (model/tool/verify/retry/parallel/delegate) \
             but declares no `limits`"
                .into(),
        ));
    };
    if limits.max_steps.is_none()
        && limits.max_model_turns.is_none()
        && limits.max_tool_calls.is_none()
    {
        return Err(HarnessError::Invalid(
            "`limits` must set at least one of `max_steps`, `max_model_turns`, `max_tool_calls`"
                .into(),
        ));
    }
    // Fan-out is static: a parallel step's branch count is known at
    // compile time, so a fan-out wider than the declared `max_parallel`
    // is rejected offline rather than failing mid-run.
    if let Some(max) = limits.max_parallel {
        for (id, step) in &spec.steps {
            if let StepSpec::Parallel(p) = step {
                let branches = p.branches.len() as u64;
                if branches > max {
                    return Err(HarnessError::Invalid(format!(
                        "step `{id}` fans out to {branches} branches but `max_parallel` is {max}"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_outputs(spec: &HarnessSpec) -> Result<(), HarnessError> {
    let Some(outputs) = &spec.outputs else {
        return Ok(());
    };
    for (id, step) in &spec.steps {
        let StepSpec::Return(r) = step else {
            continue;
        };
        let serde_json::Value::Object(map) = &r.value else {
            return Err(HarnessError::Invalid(format!(
                "step `{id}`: `return.value` must be an object matching the output contract"
            )));
        };
        for (name, decl) in outputs {
            let Some(value) = map.get(name) else {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}`: `return.value` is missing output `{name}`"
                )));
            };
            // A template that is not a single hole produces a string at run
            // time; a single hole carries the referenced value's type. Both
            // compile; a literal is checked now.
            if let serde_json::Value::String(text) = value {
                if Template::parse(text)?.is_single_hole() {
                    continue;
                }
            }
            if !decl.ty.matches(value) {
                return Err(HarnessError::Invalid(format!(
                    "step `{id}`: output `{name}` is declared {} but the value is {value}",
                    serde_json::to_string(&decl.ty).unwrap_or_default()
                )));
            }
        }
    }
    Ok(())
}
