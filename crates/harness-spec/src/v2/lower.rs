// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lowering a validated workflow into a version 1 [`HarnessSpec`].
//!
//! Each task becomes: an optional `when` branch, an `agent` step for its
//! work turn, an `agent` step (behind an optional branch) per follow-up
//! turn, then its checks with their repair attempts (see [`super::checks`]).
//! The tasks are chained in order, the success condition follows the last
//! one, and `complete` returns the declared outputs. Step ids are
//! `<task>__<role>...`; the source map records what each one is for.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};

use super::checks::{
    lower_checks, Builder, Kind, Phase, RepairPlan, Resolved, DEFAULT_REPAIR_PROMPT,
};
use super::context::{Context, LoopVars};
use super::corrections::{lower_corrections, Turn};
use super::rewrite::{
    output_fact, pool_fact, rewrite_condition, rewrite_text, rewrite_value, stop_fact, Scope,
};
use super::spec::{decoding_of, GateDef, OnFailure, TaskSpec, WorkflowSpec};
use super::validate::needed_capabilities;
use crate::error::HarnessError;
use crate::source_map::{Lowering, SourceRef};
use crate::spec::{
    AgentStepSpec, BranchStepSpec, CapabilityName, DelegateStepSpec, FailStepSpec, ForEachSpec,
    HarnessSpec, InputValue, IterateStepSpec, LimitsSpec, PredictStepSpec, ReturnStepSpec,
    StepSpec, UntilSpec, DEFAULT_MAX_ITEMS, WORKFLOW_API_VERSION,
};

/// The id of the step the run returns from.
const COMPLETE: &str = "complete";

fn origin(task: Option<&str>, role: &str) -> SourceRef {
    SourceRef {
        task: task.map(str::to_string),
        role: role.to_string(),
        check: None,
        attempt: None,
    }
}

/// The id of the task's loop step.
fn iter_id(task: &TaskSpec) -> String {
    format!("{}__iter", task.name)
}

/// The first step that runs for a task.
fn entry_of(task: &TaskSpec) -> String {
    if task.when.is_some() {
        format!("{}__when", task.name)
    } else if task.for_each.is_some() || task.until.is_some() {
        iter_id(task)
    } else {
        format!("{}__work", task.name)
    }
}

/// The fact an iteration's summary is stored under.
fn summary_fact(task: &TaskSpec) -> String {
    format!("work_{}", task.name)
}

/// Lowers a task's loop step, if it has a loop, whose body starts at `work`.
fn lower_loop(
    b: &mut Builder,
    task: &TaskSpec,
    work: &str,
    next: &str,
) -> Result<(), HarnessError> {
    let id = iter_id(task);
    let at = origin(Some(&task.name), "loop");
    if let Some(each) = &task.for_each {
        b.add(
            &id,
            StepSpec::Iterate(Box::new(IterateStepSpec {
                for_each: Some(ForEachSpec {
                    source: each.source.clone(),
                    id: each.id.clone(),
                    max_items: each.max_items.unwrap_or(DEFAULT_MAX_ITEMS),
                    reconcile: each.reconcile,
                    checkpoint_each_item: task.checkpoint.is_some(),
                }),
                until: None,
                body: work.to_string(),
                then: next.to_string(),
                on_exhausted: None,
                store_as: Some(summary_fact(task)),
            })),
            at.clone(),
        );
    }
    if let Some(until) = &task.until {
        let exhausted = format!("{}__exhausted", task.name);
        b.add(
            &id,
            StepSpec::Iterate(Box::new(IterateStepSpec {
                for_each: None,
                until: Some(UntilSpec {
                    max_iterations: until.max_iterations,
                    max_iterations_input: None,
                }),
                body: work.to_string(),
                then: next.to_string(),
                on_exhausted: Some(exhausted.clone()),
                store_as: None,
            })),
            at.clone(),
        );
        b.add(
            &exhausted,
            StepSpec::Fail(Box::new(FailStepSpec {
                code: Some("until_exhausted".to_string()),
                message: Some(format!(
                    "task `{}`: {} did not reach {} {} within {} pass(es)",
                    task.name,
                    until.metric,
                    until.threshold.cmp.as_str(),
                    until.threshold.value,
                    until.max_iterations
                )),
            })),
            SourceRef {
                role: "failure".to_string(),
                ..at
            },
        );
    }
    Ok(())
}

/// Lowers one task, whose last step continues at `next`.
fn lower_task(
    cx: &Context<'_>,
    b: &mut Builder,
    index: usize,
    next: &str,
) -> Result<(), HarnessError> {
    let w = cx.w;
    let task = &w.workflow[index];
    let names: Vec<String> = w.workflow.iter().map(|t| t.name.clone()).collect();
    let before = &names[..index];
    let through = &names[..=index];
    let place = format!("task `{}`", task.name);
    let vars = LoopVars::of(task);
    let tools = task.tools.clone().or_else(|| w.agent.tools.clone());
    let decoding = decoding_of(&w.agent.sampling, &w.agent.thinking, &w.agent.turn)
        .overlaid_by(&decoding_of(&task.sampling, &task.thinking, &task.turn));
    let fact = output_fact(&task.name);
    let role = task.model.clone().or_else(|| w.agent.model.clone());
    let agent_system = w
        .agent
        .system
        .as_deref()
        .map(|text| rewrite_text(text, &cx.scope("agent.system".into(), &[], vars)))
        .transpose()?;
    let task_system = task
        .system
        .as_deref()
        .map(|text| rewrite_text(text, &cx.scope(format!("{place}: system"), before, vars)))
        .transpose()?;
    if let Some(offered) = super::imports::offered(w, task) {
        return lower_use(cx, b, task, &offered.name, next);
    }
    let looping = task.for_each.is_some() || task.until.is_some();

    // What the body hands over to when it is done: the loop step, which
    // decides whether another pass or item follows, or the next task.
    let after_body = if looping {
        iter_id(task)
    } else {
        next.to_string()
    };
    // An `until` task checks its metric after the body; a pass leaves the
    // loop, a miss returns to the loop step, which counts it.
    let after_checks = match &task.until {
        Some(until) => {
            let metric = cx.metric(&until.metric, until.threshold, vars, through)?;
            let phase = Phase {
                prefix: format!("{}__until", task.name),
                task: Some(task.name.clone()),
                fail_code: "until_failed",
                what: place.clone(),
                place: format!("{place}: until"),
                repair: None,
                loop_back: Some(iter_id(task)),
            };
            lower_checks(b, &phase, &[metric], next)?
        }
        None => after_body.clone(),
    };

    let what = if task.for_each.is_some() {
        format!("{place} item `{{{{ facts.item_id }}}}`")
    } else {
        place.clone()
    };
    let checks = cx.task_checks(task, through)?;
    let repair = task.repair.as_ref().map(|r| RepairPlan {
        attempts: u32::try_from(r.attempts).unwrap_or(u32::MAX),
        prompt: r
            .prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_REPAIR_PROMPT.to_string()),
        tools: tools.clone(),
        fact: fact.clone(),
        inputs: cx.inputs.as_ref(),
        tasks: through,
        item: vars.item,
        iteration: vars.iteration,
        decoding: decoding.clone(),
        model_only: task
            .model_only
            .then(|| task.schema.is_some().then(|| task.name.clone())),
        role: role.clone(),
        stop: (!task.model_only).then(|| stop_fact(&task.name)),
        pool: (!task.model_only).then(|| pool_fact(&task.name)),
        max_repeated_results: task.max_repeated_results,
        conversation: task.model_only.then(|| task.name.clone()),
        system: if task.model_only {
            task_system.clone()
        } else {
            agent_system.clone()
        },
        structured: &cx.structured,
        gates: &cx.command_gates,
    });
    let phase = Phase {
        prefix: task.name.clone(),
        task: Some(task.name.clone()),
        fail_code: "gate_failed",
        what,
        place: place.clone(),
        repair,
        loop_back: None,
    };
    // The corrections come after the task's own checks and before the loop or
    // the next task.
    let after_checks = match &task.corrections {
        Some(c) => {
            let mut visible: Vec<String> = through.to_vec();
            visible.extend(cx.authors.iter().cloned());
            lower_corrections(
                b,
                c,
                &Turn {
                    cx,
                    task,
                    vars,
                    visible: &visible,
                    tools: tools.clone(),
                    agent_system: agent_system.clone(),
                    role: role.clone(),
                    decoding: decoding.clone(),
                    stop: stop_fact(&task.name),
                    pool: pool_fact(&task.name),
                    exit: &after_checks,
                },
            )?
        }
        None => after_checks,
    };
    let mut following = lower_checks(b, &phase, &checks, &after_checks)?;

    let stop = stop_fact(&task.name);
    let pool = pool_fact(&task.name);
    let allowance =
        |value: &Option<Value>, scope: &Scope<'_>| -> Result<Option<String>, HarnessError> {
            value
                .as_ref()
                .map(|v| match v {
                    Value::String(text) => rewrite_text(text, scope),
                    other => Ok(other.to_string()),
                })
                .transpose()
        };
    for (j, turn) in task.follow_ups.iter().enumerate().rev() {
        let scope = cx.scope(format!("{place}: follow_ups[{j}]"), through, vars);
        let times = turn.max.unwrap_or(1);
        // The turn is sent up to `max` times, each only while its condition
        // holds; the condition is read again after every turn.
        for k in (0..times).rev() {
            let id = if k == 0 {
                format!("{}__f{j}", task.name)
            } else {
                format!("{}__f{j}_{k}", task.name)
            };
            b.add(
                &id,
                StepSpec::Agent(Box::new(AgentStepSpec {
                    prompt: rewrite_text(&turn.task, &scope)?,
                    tools: tools.clone(),
                    store_as: Some(fact.clone()),
                    system: agent_system.clone(),
                    origin: Some("follow_up".to_string()),
                    role: role.clone(),
                    stop_as: Some(stop.clone()),
                    pool: Some(pool.clone()),
                    allowance: if k == 0 {
                        allowance(&turn.allowance, &scope)?
                    } else {
                        None
                    },
                    max_repeated_results: task.max_repeated_results,
                    decoding: decoding.clone(),
                    then: following.clone(),
                })),
                origin(Some(&task.name), "follow_up"),
            );
            following = match &turn.when {
                None => id,
                Some(condition) => {
                    let when_id = format!("{id}_when");
                    b.add(
                        &when_id,
                        StepSpec::Branch(Box::new(BranchStepSpec {
                            on: rewrite_condition(condition, &scope, &task.name)?,
                            when_true: id,
                            when_false: following,
                        })),
                        origin(Some(&task.name), "follow_up"),
                    );
                    when_id
                }
            };
        }
    }

    let scope = cx.scope(format!("{place}: task"), before, vars);
    let task_prompt = rewrite_text(&task.task, &scope)?;
    let prompt = match &w.agent.instructions {
        Some(text) => {
            let instructions =
                rewrite_text(text, &cx.scope("agent.instructions".into(), &[], vars))?;
            format!("{}\n\n{task_prompt}", instructions.trim_end())
        }
        None => task_prompt,
    };
    let work = format!("{}__work", task.name);
    let work_step = if task.model_only {
        StepSpec::Predict(Box::new(PredictStepSpec {
            prompt,
            schema: task.schema.is_some().then(|| task.name.clone()),
            store_as: Some(fact),
            conversation: Some(task.name.clone()),
            system: task_system.clone(),
            origin: Some("task".to_string()),
            role: role.clone(),
            decoding: decoding.clone(),
            then: following,
        }))
    } else {
        StepSpec::Agent(Box::new(AgentStepSpec {
            prompt,
            tools,
            store_as: Some(fact),
            system: agent_system.clone(),
            origin: Some("task".to_string()),
            role: role.clone(),
            stop_as: Some(stop),
            pool: Some(pool),
            allowance: allowance(&task.allowance, &scope)?,
            max_repeated_results: task.max_repeated_results,
            decoding: decoding.clone(),
            then: following,
        }))
    };
    b.add(&work, work_step, origin(Some(&task.name), "work"));
    if looping {
        lower_loop(b, task, &work, next)?;
    }
    if let Some(condition) = &task.when {
        let scope = cx.scope(format!("{place}: when"), before, LoopVars::default());
        let entered = if looping { iter_id(task) } else { work };
        b.add(
            &entry_of(task),
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(condition, &scope, &task.name)?,
                when_true: entered,
                when_false: next.to_string(),
            })),
            origin(Some(&task.name), "when"),
        );
    }
    Ok(())
}

/// A `use` task: one delegate step that runs the imported workflow and keeps
/// what it returns as the task's output. A child that fails fails the run at
/// this task.
fn lower_use(
    cx: &Context<'_>,
    b: &mut Builder,
    task: &TaskSpec,
    child: &str,
    next: &str,
) -> Result<(), HarnessError> {
    let names: Vec<String> = cx.w.workflow.iter().map(|t| t.name.clone()).collect();
    let index = names.iter().position(|n| *n == task.name).unwrap_or(0);
    let before = &names[..index];
    let work = format!("{}__work", task.name);
    b.add(
        &work,
        StepSpec::Delegate(Box::new(DelegateStepSpec {
            target: child.to_string(),
            as_kind: None,
            input: super::imports::call_input(cx, task, before)?,
            store_as: Some(super::imports::result_fact(task)),
            propagate_failure: true,
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
        b.add(
            &entry_of(task),
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(condition, &scope, &task.name)?,
                when_true: work,
                when_false: next.to_string(),
            })),
            origin(Some(&task.name), "when"),
        );
    }
    Ok(())
}

fn lower_success(cx: &Context<'_>, b: &mut Builder) -> Result<String, HarnessError> {
    let w = cx.w;
    let none = LoopVars::default();
    let names: Vec<String> = w.workflow.iter().map(|t| t.name.clone()).collect();
    let mut checks = Vec::new();
    for gate in &w.success.gates {
        checks.push(cx.gate(gate, none, &names)?);
    }
    for (reference, keys) in &w.success.metrics {
        let threshold = keys
            .threshold()
            .map_err(|e| HarnessError::Invalid(format!("success.metrics.{reference}: {e}")))?;
        checks.push(cx.metric(reference, threshold, none, &names)?);
    }
    if w.success
        .work_items
        .as_ref()
        .is_some_and(|i| i.all_completed)
    {
        for task in w.workflow.iter().filter(|t| t.for_each.is_some()) {
            checks.push(Resolved {
                label: format!("work_items.{}", task.name),
                required: true,
                kind: Kind::Fact {
                    path: format!("facts.{}.pending", summary_fact(task)),
                    equals: json!(0),
                    failure: format!(
                        "work items of task `{}` did not all complete (the loop never finished)",
                        task.name
                    ),
                },
            });
        }
    }
    let phase = Phase {
        prefix: "success".to_string(),
        task: None,
        fail_code: "success_check_failed",
        what: "the success condition".to_string(),
        place: "success".to_string(),
        repair: None,
        loop_back: None,
    };
    lower_checks(b, &phase, &checks, COMPLETE)
}

fn outputs_value(cx: &Context<'_>) -> Result<Value, HarnessError> {
    let w = cx.w;
    let Some(outputs) = &w.outputs else {
        return Ok(json!({ "status": "completed" }));
    };
    let names: Vec<String> = w.workflow.iter().map(|t| t.name.clone()).collect();
    let mut value = serde_json::Map::new();
    for (name, decl) in outputs {
        let scope = cx.scope(format!("outputs.{name}"), &names, LoopVars::default());
        value.insert(name.clone(), rewrite_value(&decl.value, &scope)?);
    }
    if w.on_failure == OnFailure::Return {
        value.insert("status".into(), json!("completed"));
    }
    Ok(Value::Object(value))
}

/// The most step executions a lowered workflow can make: every step once,
/// and for a task that loops, every step of the task once per pass plus the
/// loop step's own visits.
fn step_bound(w: &WorkflowSpec, steps: &std::collections::BTreeMap<String, StepSpec>) -> u64 {
    let total = steps.len() as u64;
    let mut bound = total;
    for task in &w.workflow {
        let passes = match (&task.for_each, &task.until, &task.corrections) {
            (Some(each), _, _) => each.max_items.unwrap_or(DEFAULT_MAX_ITEMS),
            (_, Some(until), _) => until.max_iterations,
            // A correction loop runs its steps once per correction, and once
            // more to leave.
            (_, _, Some(corrections)) => corrections.max + 1,
            _ => continue,
        };
        let prefix = format!("{}__", task.name);
        let task_steps = steps.keys().filter(|id| id.starts_with(&prefix)).count() as u64;
        bound += passes.saturating_mul(task_steps + 1);
    }
    bound
}

/// With `on_failure: return`, every step that would end the run as failed
/// returns the outputs instead, marked failed and naming the failure.
fn returning_failures(cx: &Context<'_>, b: &mut Builder) -> Result<(), HarnessError> {
    let outputs = outputs_value(cx)?;
    for step in b.steps.values_mut() {
        let StepSpec::Fail(fail) = step else { continue };
        let mut value = match &outputs {
            Value::Object(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        value.insert("status".into(), json!("failed"));
        value.insert(
            "failure".into(),
            json!({ "code": fail.code, "message": fail.message }),
        );
        *step = StepSpec::Return(Box::new(ReturnStepSpec {
            value: Value::Object(value),
        }));
    }
    Ok(())
}

/// Lowers `w`, which has passed [`super::validate::validate`].
///
/// # Errors
///
/// A template that reads something the author may not: an undeclared input,
/// a task that has not run, an internal namespace.
pub(super) fn lower(w: &WorkflowSpec) -> Result<HarnessSpec, HarnessError> {
    let cx = Context {
        w,
        inputs: w.inputs.as_ref().map(|i| i.keys().cloned().collect()),
        authors: w
            .workflow
            .iter()
            .filter_map(|t| t.corrections.as_ref().map(|c| c.author.name.clone()))
            .collect(),
        command_gates: w
            .gates
            .iter()
            .filter(|(_, g)| matches!(g, GateDef::Command(_)))
            .map(|(name, _)| name.clone())
            .collect(),
        structured: w
            .workflow
            .iter()
            .filter(|t| t.schema.is_some())
            .map(|t| t.name.clone())
            .collect(),
        imported: w
            .workflow
            .iter()
            .filter_map(|t| {
                let offered = w.resolved.get(t.use_import.as_deref()?)?;
                Some((t.name.clone(), offered.outputs.keys().cloned().collect()))
            })
            .collect(),
    };
    let mut b = Builder::default();
    let mut next = lower_success(&cx, &mut b)?;
    b.add(
        COMPLETE,
        StepSpec::Return(Box::new(ReturnStepSpec {
            value: outputs_value(&cx)?,
        })),
        origin(None, "complete"),
    );
    for index in (0..w.workflow.len()).rev() {
        lower_task(&cx, &mut b, index, &next)?;
        next = entry_of(&w.workflow[index]);
    }

    if w.on_failure == OnFailure::Return {
        returning_failures(&cx, &mut b)?;
    }
    let limits = w.execution.limits.clone().unwrap_or_default();
    // A plan without loops is acyclic, so no run executes a step twice: the
    // step count is the exact bound. With loops it is the bound above. Either
    // way the author's tighter `steps` wins.
    let max_steps = limits.steps.unwrap_or_else(|| step_bound(w, &b.steps));
    let capabilities = match &w.capabilities {
        Some(declared) => declared.clone(),
        None => needed_capabilities(w)
            .into_iter()
            .map(CapabilityName)
            .collect(),
    };
    Ok(HarnessSpec {
        api_version: WORKFLOW_API_VERSION.to_string(),
        name: w.name.clone(),
        description: w.description.clone(),
        inputs: w.inputs.clone(),
        outputs: w.outputs.as_ref().map(|outputs| {
            outputs
                .iter()
                .map(|(name, decl)| {
                    (
                        name.clone(),
                        InputValue {
                            ty: decl.ty.clone(),
                            description: decl.description.clone(),
                        },
                    )
                })
                .collect()
        }),
        capabilities: Some(capabilities),
        limits: Some(LimitsSpec {
            max_steps: Some(max_steps),
            max_model_turns: limits.model_turns,
            max_tool_calls: limits.tool_calls.as_ref().and_then(Value::as_u64),
            max_tool_calls_input: limits
                .tool_calls
                .as_ref()
                .and_then(Value::as_str)
                .and_then(|text| crate::expr::Template::parse(text).ok())
                .and_then(|t| t.single_hole().map(|h| h.segments()[1..].join("."))),
            max_delegate_depth: limits.delegate_depth,
            max_parallel: None,
            max_helper_fuel: None,
            max_helper_pages: None,
        }),
        entry: next,
        steps: b.steps,
        schemas: {
            let schemas: std::collections::BTreeMap<String, Value> = w
                .workflow
                .iter()
                .filter_map(|t| t.schema.clone().map(|s| (t.name.clone(), s)))
                .collect();
            (!schemas.is_empty()).then_some(schemas)
        },
        lowering: Some(Lowering {
            profile: w.agent.profile.as_str().to_string(),
            source_map: b.source,
        }),
    })
}
