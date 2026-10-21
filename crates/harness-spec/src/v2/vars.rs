// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Run variables in the workflow language: what a document may declare and
//! grant, and what lowering emits for them.
//!
//! `variables:` declares typed values; a task's `vars:` names the ones it may
//! see (`read`), set (`write`) or must set (`required`). An agent task that
//! names any is offered the harness's variable tools, answered by the machine.
//! A `required` variable still unset when the task's turns end earns the agent
//! a reminder in its own conversation (twice at most); still unset, the run
//! fails with `missing_variable`. An `item` variable is cleared when each loop
//! item or pass begins.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde_json::json;

use super::checks::Builder;
use super::lower::origin;
use serde::Deserialize;
use serde_json::Value;

use super::spec::{TaskSpec, WorkflowSpec};
use super::validate::is_identifier;
use crate::error::HarnessError;
use crate::spec::{
    AgentStepSpec, AssignStepSpec, BranchStepSpec, FailStepSpec, IOType, StepDecoding, StepSpec,
};
use crate::vars::{var_fact, var_set_fact, VarAccess, VariableDecl, VAR_TOOLS};

/// How many times an agent is reminded of one required variable.
const REMINDERS: u32 = 2;

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// Checks the declarations and every task's grants.
pub(super) fn validate(w: &WorkflowSpec) -> Result<(), HarnessError> {
    for (name, decl) in &w.variables {
        if !is_identifier(name) || name.contains('-') {
            return Err(invalid(format!(
                "variables.{name}: a variable name is lowercase letters, digits and `_`, \
                 starting with a letter"
            )));
        }
        if let Some(schema) = &decl.schema {
            if decl.ty != IOType::Json || !schema.is_object() {
                return Err(invalid(format!(
                    "variables.{name}: `schema` is an object, for a `json` variable"
                )));
            }
        }
    }
    let lists = std::iter::once(("agent.tools".to_string(), w.agent.tools.as_ref())).chain(
        w.all_tasks()
            .into_iter()
            .map(|t| (format!("task `{}`: tools", t.name), t.tools.as_ref())),
    );
    for (place, list) in lists {
        if let Some(tool) = list
            .into_iter()
            .flatten()
            .find(|t| VAR_TOOLS.contains(&t.as_str()))
        {
            return Err(invalid(format!(
                "{place}: `{tool}` is offered automatically to a task that names variables \
                 (`vars:`); do not list it"
            )));
        }
    }
    for task in w.all_tasks() {
        let place = format!("task `{}`", task.name);
        for (name, use_) in &task.vars {
            if !w.variables.contains_key(name) {
                return Err(invalid(format!(
                    "{place}: vars names `{name}`, which is not declared under `variables`"
                )));
            }
            let agent = !task.model_only
                && !super::body::is_check_task(task)
                && task.use_import.is_none()
                && task.body.is_empty();
            if *use_ != VarUse::Read && !agent {
                return Err(invalid(format!(
                    "{place}: only an agent task can set a variable; `{name}` can be `read` \
                     here (a gate's `capture` sets one without a model)"
                )));
            }
        }
        if !task.body.is_empty() && !task.vars.is_empty() {
            return Err(invalid(format!(
                "{place}: `vars` belongs to the tasks of the `do` body"
            )));
        }
        let agent_task = !task.model_only && !super::body::is_check_task(task);
        if agent_task && !task.vars.is_empty() && task.tools.is_none() && w.agent.tools.is_none() {
            return Err(invalid(format!(
                "{place}: a task that names variables needs a `tools` list (on the task or the \
                 agent); the variable tools are added to it"
            )));
        }
    }
    Ok(())
}

/// The declarations as the plan carries them.
pub(super) fn decls(w: &WorkflowSpec) -> BTreeMap<String, VariableDecl> {
    w.variables
        .iter()
        .map(|(name, v)| {
            (
                name.clone(),
                VariableDecl {
                    ty: v.ty.clone(),
                    schema: v.schema.clone(),
                    description: v.description.clone(),
                },
            )
        })
        .collect()
}

/// What the task's agent steps may do with each variable it names.
pub(super) fn grants(task: &TaskSpec) -> BTreeMap<String, VarAccess> {
    task.vars
        .iter()
        .map(|(name, use_)| {
            let access = match use_ {
                VarUse::Read => VarAccess::Read,
                VarUse::Write | VarUse::Required => VarAccess::Write,
            };
            (name.clone(), access)
        })
        .collect()
}

/// The task's tool list with the variable tools added when it names any.
pub(super) fn tools_for(task: &TaskSpec, tools: Option<Vec<String>>) -> Option<Vec<String>> {
    if task.vars.is_empty() {
        return tools;
    }
    let mut tools = tools.unwrap_or_default();
    tools.extend(VAR_TOOLS.iter().map(|t| (*t).to_string()));
    Some(tools)
}

/// What a reminder turn is sent with: the task's own turn settings.
pub(super) struct Reminder {
    pub tools: Option<Vec<String>>,
    pub conversation: String,
    pub system: Option<String>,
    pub role: Option<String>,
    pub decoding: StepDecoding,
    pub stop: String,
    pub pool: String,
    pub max_repeated_results: Option<u32>,
}

/// Lowers the checks of the task's required variables, which run after its
/// turns and before its checks; returns the first step (`next` when the task
/// requires nothing).
pub(super) fn lower_required(
    w: &WorkflowSpec,
    b: &mut Builder,
    task: &TaskSpec,
    turn: &Reminder,
    item_failure: Option<&str>,
    next: String,
) -> String {
    let mut following = next;
    let required: Vec<&String> = task
        .vars
        .iter()
        .filter(|(_, use_)| **use_ == VarUse::Required)
        .map(|(name, _)| name)
        .collect();
    for name in required.into_iter().rev() {
        let decl = &w.variables[name];
        let at = origin(Some(&task.name), "variables");
        let fail = format!("{}__var_{name}_fail", task.name);
        let message = format!(
            "task `{}`: the required variable `{name}` was not set (the agent was reminded \
             {REMINDERS} times)",
            task.name
        );
        let step = match item_failure {
            Some(target) => super::checks::fail_item(&message, target),
            None => StepSpec::Fail(Box::new(FailStepSpec {
                code: Some("missing_variable".to_string()),
                message: Some(message),
            })),
        };
        b.add(&fail, step, at.clone());
        let mut on_missing = fail;
        for k in (1..=REMINDERS).rev() {
            let check = format!("{}__var_{name}_c{k}", task.name);
            b.add(
                &check,
                StepSpec::Branch(Box::new(BranchStepSpec {
                    on: json!(format!("facts.{}", var_set_fact(name))),
                    when_true: following.clone(),
                    when_false: on_missing,
                })),
                at.clone(),
            );
            let remind = format!("{}__var_{name}_r{k}", task.name);
            b.add(
                &remind,
                StepSpec::Agent(Box::new(AgentStepSpec {
                    prompt: reminder_text(name, &decl.ty, decl.description.as_deref()),
                    tools: turn.tools.clone(),
                    store_as: None,
                    conversation: Some(turn.conversation.clone()),
                    vars: grants(task),
                    system: turn.system.clone(),
                    origin: Some("variables".to_string()),
                    role: turn.role.clone(),
                    stop_as: Some(turn.stop.clone()),
                    pool: Some(turn.pool.clone()),
                    allowance: None,
                    max_repeated_results: turn.max_repeated_results,
                    decoding: turn.decoding.clone(),
                    then: check,
                })),
                at.clone(),
            );
            on_missing = remind;
        }
        let entry = format!("{}__var_{name}", task.name);
        b.add(
            &entry,
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: json!(format!("facts.{}", var_set_fact(name))),
                when_true: following,
                when_false: on_missing,
            })),
            at,
        );
        following = entry;
    }
    following
}

fn reminder_text(name: &str, ty: &IOType, description: Option<&str>) -> String {
    let ty = match ty {
        IOType::String => "a string",
        IOType::Number => "a number",
        IOType::Boolean => "a boolean",
        IOType::Json => "a JSON value",
    };
    let what = description.map_or_else(String::new, |d| format!(" ({d})"));
    format!(
        "Your task is not finished: the required variable `{name}`{what} is not set. Set it \
         now with the harness_vars_set tool (name \"{name}\", value: {ty}). Do not redo or \
         change any other work."
    )
}

/// When the workflow declares `item` variables, a step at the start of each
/// loop item (or pass) that clears them, continuing at `body`; returns the
/// body's new entry.
pub(super) fn lower_reset(
    w: &WorkflowSpec,
    b: &mut Builder,
    owner: &TaskSpec,
    body: &str,
) -> String {
    let item: Vec<&String> = w
        .variables
        .iter()
        .filter(|(_, v)| v.scope == VarScope::Item)
        .map(|(name, _)| name)
        .collect();
    if item.is_empty() {
        return body.to_string();
    }
    let id = format!("{}__reset", owner.name);
    let mut set = BTreeMap::new();
    for name in item {
        set.insert(var_fact(name), json!(null));
        set.insert(var_set_fact(name), json!(false));
    }
    b.add(
        &id,
        StepSpec::Assign(Box::new(AssignStepSpec {
            set,
            then: body.to_string(),
        })),
        origin(Some(&owner.name), "loop"),
    );
    id
}

/// A declared variable (`variables.<name>`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableSpec {
    /// `string`, `number`, `boolean` or `json`.
    #[serde(rename = "type")]
    pub ty: IOType,
    /// `run` (the default): kept for the whole run; `item`: cleared when
    /// each `for_each` item (or `until` pass) begins.
    #[serde(default)]
    pub scope: VarScope,
    /// For a `json` variable, the shape its value must have.
    #[serde(default)]
    pub schema: Option<Value>,
    /// What the variable is for; agents see it when they list variables.
    #[serde(default)]
    pub description: Option<String>,
}

/// How long a variable's value lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarScope {
    /// The whole run.
    #[default]
    Run,
    /// One item (or pass) of a loop.
    Item,
}

/// What a task may do with a variable (`vars: {name: read | write | required}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarUse {
    /// Read it.
    Read,
    /// Read and set it.
    Write,
    /// Read and set it, and it must be set when the task's turns end: the
    /// agent is reminded, and the run fails if it still is not.
    Required,
}
