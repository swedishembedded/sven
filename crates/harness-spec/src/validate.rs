// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Static validation: everything that can be rejected before a plan exists.
//!
//! Validation is total over the typed spec — every step, every reference,
//! every declared contract — because a harness must fail offline, at
//! `validate` time, never halfway through a run. The checks:
//!
//! 1. `api_version` is supported.
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
use crate::spec::{HarnessSpec, StepSpec, SUPPORTED_API_VERSION};

/// The bounded exploration depth of the reachability check: deep enough
/// that any acyclic path to a terminal is found, shallow enough that the
/// check is fast. A cycle with a genuine exit reaches a terminal well within
/// this budget; a cycle with none cannot.
const VISIT_CAP: usize = 64;

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
    if spec.api_version != SUPPORTED_API_VERSION {
        return Err(HarnessError::Invalid(format!(
            "unsupported api_version `{}` (this compiler supports `{SUPPORTED_API_VERSION}`)",
            spec.api_version
        )));
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
        _ => {}
    }
    Ok(())
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
        StepSpec::Verify(_)
        | StepSpec::Branch(_)
        | StepSpec::Retry(_)
        | StepSpec::Parallel(_)
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
        // Terminals hand control to nobody.
        StepSpec::Return(_) | StepSpec::Fail(_) => Vec::new(),
    }
}

/// Every step must be able to reach a terminal. This is what rejects
/// unbounded cycles: a cycle in which no path exits to `return`/`fail` can
/// only run away. Cycles *through* a bounded `retry` are fine because the
/// retry's attempts bound them — the walk charges a `retry`'s body edge
/// against its `attempts` budget, so a bounded cycle terminates within the
/// exploration budget while an unbounded one cannot reach a terminal at all.
fn validate_reachability(spec: &HarnessSpec) -> Result<(), HarnessError> {
    // For each step, DFS following successors within a bounded budget; a
    // genuinely unbounded cycle never reaches a terminal within it, a path
    // with an exit does.
    let is_terminal = |id: &str| {
        matches!(
            spec.steps.get(id),
            Some(StepSpec::Return(_)) | Some(StepSpec::Fail(_))
        )
    };
    let mut unreachable: Vec<String> = Vec::new();
    for start in spec.steps.keys() {
        if is_terminal(start) {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        if !can_reach_terminal(spec, start, &is_terminal, &mut seen, VISIT_CAP) {
            unreachable.push(start.clone());
        }
    }
    if !unreachable.is_empty() {
        return Err(HarnessError::Invalid(format!(
            "unbounded cycle: step(s) [{}] can never reach a `return` or `fail` step",
            unreachable.join(", ")
        )));
    }
    Ok(())
}

fn can_reach_terminal(
    spec: &HarnessSpec,
    id: &str,
    is_terminal: &dyn Fn(&str) -> bool,
    seen: &mut std::collections::BTreeSet<(String, usize)>,
    depth: usize,
) -> bool {
    if is_terminal(id) {
        return true;
    }
    if depth == 0 {
        return false;
    }
    let Some(step) = spec.steps.get(id) else {
        return false; // dangling reference; reported elsewhere
    };
    // `retry`'s body edge is bounded by attempts; walking it with a decreasing
    // budget is what keeps bounded cycles from looking unbounded.
    let budget = match step {
        StepSpec::Retry(r) => usize::try_from(r.attempts)
            .unwrap_or(usize::MAX)
            .min(VISIT_CAP),
        _ => 1,
    };
    for target in successors(step) {
        let key = (target.clone(), depth.saturating_sub(budget));
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        if can_reach_terminal(
            spec,
            &target,
            is_terminal,
            seen,
            depth.saturating_sub(budget),
        ) {
            return true;
        }
    }
    false
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
