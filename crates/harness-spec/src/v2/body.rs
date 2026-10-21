// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Loops with a `do:` body, tasks without a prompt, and the conversation each
//! task runs in.
//!
//! A loop task may carry `do: [tasks...]` instead of one prompt: for every
//! item (or pass) the body's tasks run in order, each with its own prompt,
//! tools, conversation, checks and repairs, and the loop variables in scope.
//! A task with `verify` (or `outputs`) and no prompt is a *check task*: its
//! checks run without a model turn. Every task has a conversation of its own
//! unless it names one: one per item in a `for_each`, one per pass in an
//! `until`, so no agent reads another's history.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};

use super::checks::{lower_checks, Builder, Phase};
use super::context::{Context, LoopVars};
use super::lower::{entry_of, iter_id, lower_loop, lower_task, origin};
use super::rewrite::{output_fact, pool_fact, rewrite_condition, rewrite_text, stop_fact};
use super::spec::{TaskSpec, WorkflowSpec};
use super::validate::check_metric_ref;
use crate::error::HarnessError;
use crate::spec::{
    AssignStepSpec, BranchStepSpec, IOType, IterateSource, Reconcile, StepSpec, MAX_ITEMS_LIMIT,
    MAX_ITERATIONS_LIMIT,
};
use crate::validate::is_workspace_relative;

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// The keys a loop with a `do:` body may carry besides `do`.
const OWNER_KEYS: &str = "`name`, `description`, `when`, `for_each` or `until`, and `checkpoint`";

/// Checks the shape of `task` with respect to prompts and bodies: a task has a
/// prompt, a `do:` body, a `use`, or checks to run; a body belongs to a loop
/// and holds plain tasks.
pub(super) fn validate_shape(
    w: &WorkflowSpec,
    task: &TaskSpec,
    place: &str,
) -> Result<(), HarnessError> {
    let prompt = !task.task.trim().is_empty();
    if !task.body.is_empty() {
        if task.for_each.is_none() && task.until.is_none() {
            return Err(invalid(format!(
                "{place}: `do` is the body of a loop; add `for_each` or `until`"
            )));
        }
        let extra = prompt
            || task.tools.is_some()
            || !task.follow_ups.is_empty()
            || !task.outputs.is_empty()
            || !task.verify.is_empty()
            || task.repair.is_some()
            || task.model_only
            || task.system.is_some()
            || task.model.is_some()
            || task.schema.is_some()
            || task.sampling.is_some()
            || task.thinking.is_some()
            || task.turn.is_some()
            || task.allowance.is_some()
            || task.max_repeated_results.is_some()
            || task.corrections.is_some()
            || task.use_import.is_some()
            || task.conversation.is_some();
        if extra {
            return Err(invalid(format!(
                "{place}: a loop with a `do` body takes only {OWNER_KEYS}; the prompt, tools \
                 and checks belong to the tasks of the body"
            )));
        }
        return Ok(());
    }
    if w.owner_of(&task.name).is_some()
        && (task.for_each.is_some() || task.until.is_some() || task.checkpoint.is_some())
    {
        return Err(invalid(format!(
            "{place}: a task of a `do` body runs once per item or pass of its loop; it cannot \
             loop itself"
        )));
    }
    if !prompt && task.use_import.is_none() {
        if task.verify.is_empty() && task.outputs.is_empty() {
            return Err(invalid(format!(
                "{place}: `task` is empty; a task without a prompt runs checks, so give it \
                 `verify`"
            )));
        }
        let shaped = task.tools.is_some()
            || !task.follow_ups.is_empty()
            || task.repair.is_some()
            || task.model_only
            || task.corrections.is_some()
            || task.allowance.is_some()
            || task.conversation.is_some();
        if shaped {
            return Err(invalid(format!(
                "{place}: a task without a prompt only runs its checks; it has no turns to \
                 shape (tools, follow-ups, repair, corrections, conversation)"
            )));
        }
    }
    Ok(())
}

/// Whether `task` is a check task: checks and no model turn.
pub(super) fn is_check_task(task: &TaskSpec) -> bool {
    task.task.trim().is_empty() && task.use_import.is_none() && task.body.is_empty()
}

/// The conversation `task` runs in, moved into run namespaces: the one it
/// names, else its own (per item or per pass inside a loop).
pub(super) fn conversation(
    cx: &Context<'_>,
    task: &TaskSpec,
    vars: LoopVars,
    tasks: &[String],
) -> Result<String, HarnessError> {
    let text = match &task.conversation {
        Some(name) => name.clone(),
        None if vars.item => format!("{}#{{{{ item_index }}}}", task.name),
        None if vars.iteration => format!("{}#{{{{ iteration }}}}", task.name),
        None => task.name.clone(),
    };
    rewrite_text(
        &text,
        &cx.scope(format!("task `{}`: conversation", task.name), tasks, vars),
    )
}

/// Lowers a loop whose body is `owner.body`, continuing at `next`.
pub(super) fn lower_group(
    cx: &Context<'_>,
    b: &mut Builder,
    owner: &TaskSpec,
    next: &str,
) -> Result<(), HarnessError> {
    let vars = LoopVars::of(owner);
    let place = format!("task `{}`", owner.name);
    let last = owner
        .body
        .last()
        .map_or(owner.name.as_str(), |t| t.name.as_str());
    let through = cx.through(last);
    // An `until` loop measures its metric after the body, as a single task
    // does after its own checks.
    let after_body = match &owner.until {
        Some(until) => {
            let metric = cx.metric(&until.metric, until.threshold, vars, through)?;
            let phase = Phase {
                prefix: format!("{}__until", owner.name),
                task: Some(owner.name.clone()),
                fail_code: "until_failed",
                what: place.clone(),
                place: format!("{place}: until"),
                repair: None,
                loop_back: Some(iter_id(owner)),
                item_failure: None,
            };
            lower_checks(b, &phase, &[metric], next)?
        }
        None => iter_id(owner),
    };
    let mut following = after_body;
    for task in owner.body.iter().rev() {
        lower_task(cx, b, task, &following)?;
        following = entry_of(task);
    }
    let first = cx.before(&owner.body[0].name);
    let covers = owner
        .for_each
        .iter()
        .flat_map(|each| &each.covers)
        .map(|path| rewrite_text(path, &cx.scope(format!("{place}: covers"), first, vars)))
        .collect::<Result<Vec<_>, _>>()?;
    let body = super::vars::lower_reset(cx.w, b, owner, &following);
    lower_loop(b, owner, &body, next, covers)?;
    lower_item_failure(cx, b, owner)?;
    if let Some(condition) = &owner.when {
        let scope = cx.scope(format!("{place}: when"), first, LoopVars::default());
        let skipped = format!("{}__skip", owner.name);
        let mut set = serde_json::Map::new();
        for task in &owner.body {
            set.insert(output_fact(&task.name), skipped_output(task));
            set.insert(stop_fact(&task.name), json!("skipped"));
            set.insert(pool_fact(&task.name), json!(0));
        }
        b.add(
            &skipped,
            StepSpec::Assign(Box::new(AssignStepSpec {
                set: set.into_iter().collect(),
                then: next.to_string(),
            })),
            origin(Some(&owner.name), "skipped"),
        );
        b.add(
            &entry_of(owner),
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(condition, &scope, &owner.name)?,
                when_true: iter_id(owner),
                when_false: skipped,
            })),
            origin(Some(&owner.name), "when"),
        );
    }
    Ok(())
}

/// The step that fails an item of `task`'s loop instead of the run, when
/// the loop has `on_item_failure`.
pub(super) fn item_failure_entry(w: &WorkflowSpec, task: &TaskSpec) -> Option<String> {
    let owner = w.loop_of(task);
    owner
        .for_each
        .as_ref()?
        .on_item_failure
        .as_ref()
        .map(|_| format!("{}__item_failed", owner.name))
}

/// Lowers `owner`'s item-failure path, when it has one: its checks run (a
/// rollback), then the loop goes on with the next item; a failing one fails
/// the run with `rollback_failed`.
pub(super) fn lower_item_failure(
    cx: &Context<'_>,
    b: &mut Builder,
    owner: &TaskSpec,
) -> Result<(), HarnessError> {
    let Some(failure) = owner
        .for_each
        .as_ref()
        .and_then(|e| e.on_item_failure.as_ref())
    else {
        return Ok(());
    };
    let vars = LoopVars::of(owner);
    let last = owner
        .body
        .last()
        .map_or(owner.name.as_str(), |t| t.name.as_str());
    let through = cx.through(last);
    let checks = failure
        .verify
        .iter()
        .map(|gate| cx.gate(gate, vars, through))
        .collect::<Result<Vec<_>, _>>()?;
    let phase = Phase {
        prefix: format!("{}__rollback", owner.name),
        task: Some(owner.name.clone()),
        fail_code: "rollback_failed",
        what: format!(
            "task `{}` item `{{{{ facts.item_id }}}}`: on_item_failure",
            owner.name
        ),
        place: format!("task `{}`: on_item_failure", owner.name),
        repair: None,
        loop_back: None,
        item_failure: None,
    };
    let entry = lower_checks(b, &phase, &checks, &iter_id(owner))?;
    b.add(
        &format!("{}__item_failed", owner.name),
        StepSpec::Assign(Box::new(AssignStepSpec {
            set: std::collections::BTreeMap::new(),
            then: entry,
        })),
        origin(Some(&owner.name), "failure"),
    );
    Ok(())
}

/// What a skipped task's output reads as: empty text, or an object of empty
/// strings for the fields of a structured one.
fn skipped_output(task: &TaskSpec) -> Value {
    match task
        .schema
        .as_ref()
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
    {
        Some(props) => Value::Object(props.keys().map(|k| (k.clone(), json!(""))).collect()),
        None => json!(""),
    }
}

/// `for_each` and `until`: one at most, a bounded count, a source that exists.
pub(super) fn validate_loop(
    w: &WorkflowSpec,
    task: &super::spec::TaskSpec,
    place: &str,
) -> Result<(), HarnessError> {
    if task.for_each.is_some() && task.until.is_some() {
        return Err(invalid(format!(
            "{place}: a task loops one way, `for_each` or `until`, not both"
        )));
    }
    if task.checkpoint.is_some() && task.for_each.is_none() {
        return Err(invalid(format!(
            "{place}: `checkpoint: each_item` needs a `for_each` task"
        )));
    }
    if let Some(each) = &task.for_each {
        if each.reconcile == Reconcile::Refresh && !matches!(each.source, IterateSource::Json(_)) {
            return Err(invalid(format!(
                "{place}: `reconcile: refresh` needs a file source; an input cannot change"
            )));
        }
        if each.covers.len() > crate::spec::MAX_COVERS {
            return Err(invalid(format!(
                "{place}: for_each.covers names {} paths, more than {}",
                each.covers.len(),
                crate::spec::MAX_COVERS
            )));
        }
        for path in &each.covers {
            if !is_workspace_relative(path) {
                return Err(invalid(format!(
                    "{place}: for_each.covers path `{path}` must be inside the workspace \
                     (relative, no `..`)"
                )));
            }
        }
        if let Some(max) = each.max_items {
            if max == 0 || max > MAX_ITEMS_LIMIT {
                return Err(invalid(format!(
                    "{place}: for_each.max_items {max} is outside 1..={MAX_ITEMS_LIMIT}"
                )));
            }
        }
        match &each.source {
            IterateSource::Json(json) => {
                if !is_workspace_relative(&json.file) || json.file.contains("{{") {
                    return Err(invalid(format!(
                        "{place}: for_each source file `{}` must be a literal path inside the \
                         workspace",
                        json.file
                    )));
                }
                if !json.pointer.is_empty() && !json.pointer.starts_with('/') {
                    return Err(invalid(format!(
                        "{place}: for_each pointer `{}` must be an RFC 6901 pointer such as /items",
                        json.pointer
                    )));
                }
            }
            IterateSource::Variable(name) => match w.variables.get(name) {
                Some(v) if v.ty == IOType::Json => {}
                Some(_) => {
                    return Err(invalid(format!(
                        "{place}: for_each reads variable `{name}`, which must be declared \
                         `json` (an array)"
                    )))
                }
                None => {
                    return Err(invalid(format!(
                        "{place}: for_each reads variable `{name}`, which is not declared under \
                         `variables`"
                    )))
                }
            },
            IterateSource::Input(name) => match w.inputs.as_ref().and_then(|i| i.get(name)) {
                Some(input) if input.ty == IOType::Json => {}
                Some(_) => {
                    return Err(invalid(format!(
                        "{place}: for_each reads input `{name}`, which must be declared `json` \
                         (an array)"
                    )))
                }
                None => {
                    return Err(invalid(format!(
                        "{place}: for_each reads input `{name}`, which is not declared"
                    )))
                }
            },
        }
        if each.id.as_deref().is_some_and(str::is_empty) {
            return Err(invalid(format!(
                "{place}: for_each.id names a field; it is empty"
            )));
        }
    }
    if let Some(until) = &task.until {
        check_metric_ref(w, &format!("{place}: until"), &until.metric)?;
        if until.max_iterations == 0 || until.max_iterations > MAX_ITERATIONS_LIMIT {
            return Err(invalid(format!(
                "{place}: until.max_iterations {} is outside 1..={MAX_ITERATIONS_LIMIT}",
                until.max_iterations
            )));
        }
    }
    Ok(())
}
