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

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::checks::{
    lower_checks, Builder, CommandRun, Kind, Phase, RepairPlan, Resolved, DEFAULT_REPAIR_PROMPT,
};
use super::rewrite::{output_fact, rewrite_condition, rewrite_text, rewrite_value, Scope};
use super::spec::{decoding_of, Check, CommandGate, GateDef, OnFailure, TaskSpec, WorkflowSpec};
use super::validate::{needed_capabilities, split_metric};
use crate::error::HarnessError;
use crate::source_map::{Lowering, SourceRef};
use crate::spec::{
    AgentStepSpec, BranchStepSpec, CapabilityName, FailStepSpec, ForEachSpec, HarnessSpec,
    InputValue, IterateStepSpec, LimitsSpec, PredictStepSpec, ReturnStepSpec, StepSpec, UntilSpec,
    DEFAULT_GATE_TIMEOUT_SECS, DEFAULT_MAX_ITEMS, WORKFLOW_API_VERSION,
};

/// The id of the step the run returns from.
const COMPLETE: &str = "complete";

struct Context<'a> {
    w: &'a WorkflowSpec,
    inputs: Option<BTreeSet<String>>,
    /// Tasks with a `schema`, whose output fields templates can read.
    structured: Vec<String>,
    /// Command gates, whose latest result templates can read.
    command_gates: Vec<String>,
}

/// What a task's templates may read besides inputs and earlier outputs.
#[derive(Clone, Copy, Default)]
struct LoopVars {
    /// `item`, `item_id` and `item_index`: inside a `for_each`.
    item: bool,
    /// `iteration`: inside an `until`.
    iteration: bool,
}

impl LoopVars {
    fn of(task: &TaskSpec) -> Self {
        Self {
            item: task.for_each.is_some(),
            iteration: task.until.is_some(),
        }
    }
}

impl Context<'_> {
    fn scope<'s>(&'s self, place: String, tasks: &'s [String], vars: LoopVars) -> Scope<'s> {
        Scope::new(place, self.inputs.as_ref(), tasks)
            .in_loop(vars.item, vars.iteration)
            .with_structured(&self.structured)
            .with_gates(&self.command_gates)
    }

    fn command_run(
        &self,
        name: &str,
        gate: &CommandGate,
        vars: LoopVars,
        tasks: &[String],
    ) -> Result<CommandRun, HarnessError> {
        let scope = self.scope(format!("gate `{name}`"), &[], vars);
        Ok(CommandRun {
            argv: gate
                .command
                .iter()
                .map(|arg| rewrite_text(arg, &scope))
                .collect::<Result<_, _>>()?,
            timeout: gate.timeout.unwrap_or(DEFAULT_GATE_TIMEOUT_SECS),
            cwd: gate.cwd.clone(),
            stdin: gate
                .stdin
                .as_deref()
                .map(|text| {
                    rewrite_text(
                        text,
                        &self.scope(format!("gate `{name}`: stdin"), tasks, vars),
                    )
                })
                .transpose()?,
            store_as: format!("gate_{name}"),
        })
    }

    fn gate(&self, name: &str, vars: LoopVars, tasks: &[String]) -> Result<Resolved, HarnessError> {
        let kind = match self.w.gates.get(name) {
            Some(GateDef::Command(c)) => Kind::Command(self.command_run(name, c, vars, tasks)?),
            Some(GateDef::Artifact(a)) => Kind::File {
                path: a.artifact.clone(),
                min_bytes: a.min_bytes,
                sha256: a.sha256.clone(),
            },
            None => {
                return Err(HarnessError::Invalid(format!(
                    "internal: gate `{name}` vanished"
                )))
            }
        };
        Ok(Resolved {
            label: name.to_string(),
            required: true,
            kind,
        })
    }

    fn metric(
        &self,
        reference: &str,
        threshold: super::spec::Threshold,
        vars: LoopVars,
        tasks: &[String],
    ) -> Result<Resolved, HarnessError> {
        let (gate, metric) = split_metric(reference)
            .ok_or_else(|| HarnessError::Invalid(format!("internal: bad metric `{reference}`")))?;
        let Some(GateDef::Command(command)) = self.w.gates.get(gate) else {
            return Err(HarnessError::Invalid(format!(
                "internal: metric `{reference}` has no command gate"
            )));
        };
        let decl = command.metrics.get(metric).ok_or_else(|| {
            HarnessError::Invalid(format!("internal: metric `{reference}` is undeclared"))
        })?;
        // A metric is only trusted from a run of its command that happened
        // for this check: a number left by an earlier run is not evidence.
        Ok(Resolved {
            label: reference.to_string(),
            required: true,
            kind: Kind::Metric {
                run: Some(self.command_run(gate, command, vars, tasks)?),
                file: decl.file.clone(),
                pointer: decl.pointer.clone(),
                threshold,
            },
        })
    }

    fn task_checks(
        &self,
        task: &TaskSpec,
        tasks: &[String],
    ) -> Result<Vec<Resolved>, HarnessError> {
        let vars = LoopVars::of(task);
        let names: Vec<String> = self.w.workflow.iter().map(|t| t.name.clone()).collect();
        let scope = self.scope(format!("task `{}`: files", task.name), &names, vars);
        let file = |path: &str| rewrite_text(path, &scope);
        let mut checks: Vec<Resolved> = Vec::new();
        for path in &task.outputs {
            checks.push(Resolved {
                label: plain(path),
                required: true,
                kind: Kind::File {
                    path: file(path)?,
                    min_bytes: None,
                    sha256: None,
                },
            });
        }
        for check in &task.verify {
            checks.push(match check {
                Check::Gate(name) => self.gate(name, vars, tasks)?,
                Check::Soft(name) => Resolved {
                    required: false,
                    ..self.gate(name, vars, tasks)?
                },
                Check::Artifact(a) => Resolved {
                    label: plain(&a.artifact),
                    required: true,
                    kind: Kind::File {
                        path: file(&a.artifact)?,
                        min_bytes: a.min_bytes,
                        sha256: a.sha256.clone(),
                    },
                },
                Check::Metric(m) => self.metric(&m.metric, m.threshold, vars, tasks)?,
            });
        }
        Ok(checks)
    }
}

/// A path as it is named in messages: a template's holes become `(...)`, so
/// that the name can be written into a message template without being read as
/// holes again.
fn plain(path: &str) -> String {
    path.replace("{{", "(").replace("}}", ")")
}

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
    let mut following = lower_checks(b, &phase, &checks, &after_checks)?;

    for (j, turn) in task.follow_ups.iter().enumerate().rev() {
        let id = format!("{}__f{j}", task.name);
        let scope = cx.scope(format!("{place}: follow_ups[{j}]"), through, vars);
        b.add(
            &id,
            StepSpec::Agent(Box::new(AgentStepSpec {
                prompt: rewrite_text(&turn.task, &scope)?,
                tools: tools.clone(),
                store_as: Some(fact.clone()),
                system: agent_system.clone(),
                origin: Some("follow_up".to_string()),
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
                        on: rewrite_condition(condition, &scope)?,
                        when_true: id,
                        when_false: following,
                    })),
                    origin(Some(&task.name), "follow_up"),
                );
                when_id
            }
        };
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
                on: rewrite_condition(condition, &scope)?,
                when_true: entered,
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
        let passes = match (&task.for_each, &task.until) {
            (Some(each), _) => each.max_items.unwrap_or(DEFAULT_MAX_ITEMS),
            (_, Some(until)) => until.max_iterations,
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
            .map(|c| CapabilityName(c.to_string()))
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
            max_tool_calls: limits.tool_calls,
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
