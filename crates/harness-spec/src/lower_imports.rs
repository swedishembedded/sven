// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `imports:` and `use:`: a task that runs another workflow.
//!
//! A document names the documents it uses (`imports: {review: ./review.yaml}`)
//! and a task runs one (`use: review`, `with: {repo: "{{ inputs.repo }}"}`).
//! The package resolves each import to its [`Imported`] interface before the
//! document is lowered, so everything about the call is checked at compile
//! time: the alias exists, `with` passes exactly the inputs the child takes
//! with types that fit, the capabilities the child holds are held by the
//! caller too, and later templates read only outputs the child returns. A
//! `use` lowers to one `delegate` step that fails the caller when the child
//! fails.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::error::HarnessError;
use crate::expr::Template;
use crate::lower::{entry_of, origin, skip_step};
use crate::lower_checks::Builder;
use crate::lower_context::{Context, LoopVars};
use crate::rewrite::{output_fact, rewrite_condition, rewrite_value};
use crate::steps::{BranchStepSpec, DelegateStepSpec, IOType, InputValue, StepSpec};
use crate::workflow::{Imported, TaskSpec, WorkflowSpec};
use crate::workflow_validate::is_identifier;

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// Checks the `imports` section and every `use` task against the resolved
/// interfaces.
pub(super) fn validate_imports(w: &WorkflowSpec) -> Result<(), HarnessError> {
    for (alias, path) in &w.imports {
        if !is_identifier(alias) {
            return Err(invalid(format!(
                "import `{alias}`: aliases are lowercase letters, digits, `-` and `_`, start \
                 with a letter and have no `__`"
            )));
        }
        if !w.resolved.contains_key(alias) {
            return Err(invalid(format!(
                "import `{alias}` ({path}) is not resolved: a document with imports compiles \
                 only as a package (a directory, or a bundle that carries the imported files)"
            )));
        }
    }
    for task in w.all_tasks() {
        match &task.use_import {
            Some(alias) => validate_use(w, task, alias)?,
            None if !task.with.is_empty() => {
                return Err(invalid(format!(
                    "task `{}`: `with` belongs to a task that has `use`",
                    task.name
                )))
            }
            None => {}
        }
    }
    let used: BTreeSet<&str> = w
        .all_tasks()
        .into_iter()
        .filter_map(|t| t.use_import.as_deref())
        .collect();
    if let Some(unused) = w.imports.keys().find(|a| !used.contains(a.as_str())) {
        return Err(invalid(format!(
            "import `{unused}` is not used by any task (`use: {unused}`)"
        )));
    }
    check_capabilities(w)
}

/// A `use` task is only a call: nothing that shapes an agent turn belongs to it.
fn validate_use(w: &WorkflowSpec, task: &TaskSpec, alias: &str) -> Result<(), HarnessError> {
    let place = format!("task `{}`", task.name);
    let offered = w.resolved.get(alias).ok_or_else(|| {
        invalid(format!(
            "{place}: `use: {alias}` names no import (imports: {})",
            if w.imports.is_empty() {
                "none".to_string()
            } else {
                w.imports.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        ))
    })?;
    let stray = [
        ("task", !task.task.trim().is_empty()),
        ("tools", task.tools.is_some()),
        ("follow_ups", !task.follow_ups.is_empty()),
        ("outputs", !task.outputs.is_empty()),
        ("verify", !task.verify.is_empty()),
        ("repair", task.repair.is_some()),
        ("for_each", task.for_each.is_some()),
        ("until", task.until.is_some()),
        ("model_only", task.model_only),
        ("system", task.system.is_some()),
        ("model", task.model.is_some()),
        ("schema", task.schema.is_some()),
        ("sampling", task.sampling.is_some()),
        ("thinking", task.thinking.is_some()),
        ("turn", task.turn.is_some()),
        ("allowance", task.allowance.is_some()),
        ("max_repeated_results", task.max_repeated_results.is_some()),
        ("attach", !task.attach.is_empty()),
        ("corrections", task.corrections.is_some()),
        ("checkpoint", task.checkpoint.is_some()),
    ];
    if let Some((key, _)) = stray.iter().find(|(_, set)| *set) {
        return Err(invalid(format!(
            "{place}: `{key}` does not go with `use`: a task that uses an imported workflow \
             takes only `use`, `with`, `when` and `description`"
        )));
    }
    for (name, declared) in &offered.inputs {
        let Some(value) = task.with.get(name) else {
            return Err(invalid(format!(
                "{place}: `{alias}` takes input `{name}`, which `with` does not pass"
            )));
        };
        check_value(w, &place, name, &declared.ty, value)?;
    }
    if let Some(extra) = task.with.keys().find(|k| !offered.inputs.contains_key(*k)) {
        return Err(invalid(format!(
            "{place}: `with` passes `{extra}`, which `{alias}` does not take (it takes: {})",
            if offered.inputs.is_empty() {
                "none".to_string()
            } else {
                offered
                    .inputs
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )));
    }
    Ok(())
}

/// A value passed to a child input must fit the input's type where that can
/// be known now: a literal by its JSON type, a single `{{ inputs.x }}` by the
/// declared type of `x`, any other template as text.
fn check_value(
    w: &WorkflowSpec,
    place: &str,
    name: &str,
    wanted: &IOType,
    value: &Value,
) -> Result<(), HarnessError> {
    let mismatch = |found: &str| {
        invalid(format!(
            "{place}: input `{name}` is {wanted:?} but `with` passes {found}"
        ))
    };
    let Value::String(text) = value else {
        return if wanted.matches(value) {
            Ok(())
        } else {
            Err(mismatch(&format!("`{value}`")))
        };
    };
    let Ok(template) = Template::parse(text) else {
        return Ok(());
    };
    if let Some(hole) = template.single_hole() {
        let segments = hole.segments();
        if let [root, input] = segments {
            let declared: Option<&InputValue> = (root == "inputs")
                .then(|| w.inputs.as_ref().and_then(|i| i.get(input)))
                .flatten();
            if let Some(declared) = declared {
                if declared.ty != *wanted && *wanted != IOType::Json && declared.ty != IOType::Json
                {
                    return Err(mismatch(&format!(
                        "input `{input}` of type {:?}",
                        declared.ty
                    )));
                }
            }
        }
        return Ok(());
    }
    // Text with a hole inside it is text.
    if matches!(wanted, IOType::String | IOType::Json)
        || !text.contains("{{") && wanted.matches(value)
    {
        Ok(())
    } else {
        Err(mismatch(&format!("text `{text}`")))
    }
}

/// What the callee holds, the caller must hold: a child runs with the
/// caller's grant narrowed by its own, so a capability the caller lacks would
/// silently never reach it.
fn check_capabilities(w: &WorkflowSpec) -> Result<(), HarnessError> {
    let Some(declared) = &w.capabilities else {
        return Ok(());
    };
    let declared: BTreeSet<&str> = declared.iter().map(|c| c.as_str()).collect();
    for task in w.all_tasks() {
        let Some(offered) = task.use_import.as_deref().and_then(|a| w.resolved.get(a)) else {
            continue;
        };
        if let Some(missing) = offered
            .capabilities
            .iter()
            .find(|c| !declared.contains(c.as_str()))
        {
            return Err(invalid(format!(
                "task `{}` uses `{}`, which needs {missing}; the workflow's `capabilities` do not \
                 include it (a child never holds more than its caller)",
                task.name, offered.name
            )));
        }
    }
    Ok(())
}

/// The capabilities a workflow's imports add to what it needs itself.
pub(super) fn imported_capabilities(w: &WorkflowSpec) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    for task in w.all_tasks() {
        if let Some(offered) = task.use_import.as_deref().and_then(|a| w.resolved.get(a)) {
            needed.insert("SpawnChild".to_string());
            needed.extend(offered.capabilities.iter().cloned());
        }
    }
    needed
}

/// The delegate input of a `use`: `with`, its templates rewritten.
pub(super) fn call_input(
    cx: &Context<'_>,
    task: &TaskSpec,
    before: &[String],
) -> Result<Value, HarnessError> {
    let scope = cx.scope(
        format!("task `{}`: with", task.name),
        before,
        LoopVars::default(),
    );
    let mut input = Map::new();
    for (name, value) in &task.with {
        input.insert(name.clone(), rewrite_value(value, &scope)?);
    }
    Ok(Value::Object(input))
}

/// The fact the child's returned outputs are kept under.
pub(super) fn result_fact(task: &TaskSpec) -> String {
    output_fact(&task.name)
}

/// The interface of `alias`, if the package resolved it.
pub(super) fn offered<'a>(w: &'a WorkflowSpec, task: &TaskSpec) -> Option<&'a Imported> {
    task.use_import.as_deref().and_then(|a| w.resolved.get(a))
}

/// A `use` task: one delegate step that runs the imported workflow and keeps
/// what it returns as the task's output. A child that fails fails the run at
/// this task.
pub(super) fn lower_use(
    cx: &Context<'_>,
    b: &mut Builder,
    task: &TaskSpec,
    child: &str,
    next: &str,
) -> Result<(), HarnessError> {
    let before = cx.before(&task.name);
    let work = format!("{}__work", task.name);
    b.add(
        &work,
        StepSpec::Delegate(Box::new(DelegateStepSpec {
            target: child.to_string(),
            input: crate::lower_imports::call_input(cx, task, before)?,
            store_as: Some(crate::lower_imports::result_fact(task)),
            then: next.to_string(),
        })),
        origin(Some(&task.name), "use"),
    );
    if let Some(condition) = &task.when {
        let scope = cx.scope(
            format!("task `{}`: when", task.name),
            before,
            LoopVars::default(),
        );
        let skipped = skip_step(cx, b, task, next);
        b.add(
            &entry_of(task),
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(condition, &scope, &task.name)?,
                when_true: work,
                when_false: skipped,
            })),
            origin(Some(&task.name), "when"),
        );
    }
    Ok(())
}
