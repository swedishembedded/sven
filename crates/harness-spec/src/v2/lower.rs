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
use super::spec::{Check, CommandGate, GateDef, TaskSpec, WorkflowSpec};
use super::validate::{needed_capabilities, split_metric};
use crate::error::HarnessError;
use crate::source_map::{Lowering, SourceRef};
use crate::spec::{
    AgentStepSpec, BranchStepSpec, CapabilityName, HarnessSpec, InputValue, LimitsSpec,
    ReturnStepSpec, StepSpec, DEFAULT_GATE_TIMEOUT_SECS, WORKFLOW_API_VERSION,
};

/// The id of the step the run returns from.
const COMPLETE: &str = "complete";

struct Context<'a> {
    w: &'a WorkflowSpec,
    inputs: Option<BTreeSet<String>>,
}

impl Context<'_> {
    fn scope<'s>(&'s self, place: String, tasks: &'s [String]) -> Scope<'s> {
        Scope {
            place,
            inputs: self.inputs.as_ref(),
            tasks,
            failure: None,
        }
    }

    fn command_run(&self, name: &str, gate: &CommandGate) -> Result<CommandRun, HarnessError> {
        let scope = self.scope(format!("gate `{name}`"), &[]);
        Ok(CommandRun {
            argv: gate
                .command
                .iter()
                .map(|arg| rewrite_text(arg, &scope))
                .collect::<Result<_, _>>()?,
            timeout: gate.timeout.unwrap_or(DEFAULT_GATE_TIMEOUT_SECS),
            cwd: gate.cwd.clone(),
        })
    }

    fn gate(&self, name: &str) -> Result<Resolved, HarnessError> {
        let kind = match self.w.gates.get(name) {
            Some(GateDef::Command(c)) => Kind::Command(self.command_run(name, c)?),
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
            kind,
        })
    }

    fn metric(
        &self,
        reference: &str,
        threshold: super::spec::Threshold,
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
            kind: Kind::Metric {
                run: Some(self.command_run(gate, command)?),
                file: decl.file.clone(),
                pointer: decl.pointer.clone(),
                threshold,
            },
        })
    }

    fn task_checks(&self, task: &TaskSpec) -> Result<Vec<Resolved>, HarnessError> {
        let mut checks: Vec<Resolved> = task
            .outputs
            .iter()
            .map(|path| Resolved {
                label: path.clone(),
                kind: Kind::File {
                    path: path.clone(),
                    min_bytes: None,
                    sha256: None,
                },
            })
            .collect();
        for check in &task.verify {
            checks.push(match check {
                Check::Gate(name) => self.gate(name)?,
                Check::Artifact(a) => Resolved {
                    label: a.artifact.clone(),
                    kind: Kind::File {
                        path: a.artifact.clone(),
                        min_bytes: a.min_bytes,
                        sha256: a.sha256.clone(),
                    },
                },
                Check::Metric(m) => self.metric(&m.metric, m.threshold)?,
            });
        }
        Ok(checks)
    }
}

fn origin(task: Option<&str>, role: &str) -> SourceRef {
    SourceRef {
        task: task.map(str::to_string),
        role: role.to_string(),
        check: None,
        attempt: None,
    }
}

fn entry_of(task: &TaskSpec) -> String {
    if task.when.is_some() {
        format!("{}__when", task.name)
    } else {
        format!("{}__work", task.name)
    }
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
    let tools = task.tools.clone().or_else(|| w.agent.tools.clone());
    let fact = output_fact(&task.name);

    let checks = cx.task_checks(task)?;
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
    });
    let phase = Phase {
        prefix: task.name.clone(),
        task: Some(task.name.clone()),
        fail_code: "gate_failed",
        what: place.clone(),
        repair,
    };
    let mut following = lower_checks(b, &phase, &checks, next)?;

    for (j, turn) in task.follow_ups.iter().enumerate().rev() {
        let id = format!("{}__f{j}", task.name);
        let scope = cx.scope(format!("{place}: follow_ups[{j}]"), through);
        b.add(
            &id,
            StepSpec::Agent(Box::new(AgentStepSpec {
                prompt: rewrite_text(&turn.task, &scope)?,
                tools: tools.clone(),
                store_as: Some(fact.clone()),
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

    let scope = cx.scope(format!("{place}: task"), before);
    let task_prompt = rewrite_text(&task.task, &scope)?;
    let prompt = match &w.agent.instructions {
        Some(text) => {
            let instructions = rewrite_text(text, &cx.scope("agent.instructions".into(), &[]))?;
            format!("{}\n\n{task_prompt}", instructions.trim_end())
        }
        None => task_prompt,
    };
    let work = format!("{}__work", task.name);
    b.add(
        &work,
        StepSpec::Agent(Box::new(AgentStepSpec {
            prompt,
            tools,
            store_as: Some(fact),
            then: following,
        })),
        origin(Some(&task.name), "work"),
    );
    if let Some(condition) = &task.when {
        let scope = cx.scope(format!("{place}: when"), before);
        b.add(
            &entry_of(task),
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(condition, &scope)?,
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
    let mut checks = Vec::new();
    for gate in &w.success.gates {
        checks.push(cx.gate(gate)?);
    }
    for (reference, keys) in &w.success.metrics {
        let threshold = keys
            .threshold()
            .map_err(|e| HarnessError::Invalid(format!("success.metrics.{reference}: {e}")))?;
        checks.push(cx.metric(reference, threshold)?);
    }
    let phase = Phase {
        prefix: "success".to_string(),
        task: None,
        fail_code: "success_check_failed",
        what: "the success condition".to_string(),
        repair: None,
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
        let scope = cx.scope(format!("outputs.{name}"), &names);
        value.insert(name.clone(), rewrite_value(&decl.value, &scope)?);
    }
    Ok(Value::Object(value))
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

    let limits = w.execution.limits.clone().unwrap_or_default();
    // The plan is acyclic, so no run executes a step twice: the step count
    // is the exact bound unless the author set a tighter one.
    let max_steps = limits.steps.unwrap_or(b.steps.len() as u64);
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
        schemas: None,
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
        lowering: Some(Lowering {
            profile: w.agent.profile.as_str().to_string(),
            source_map: b.source,
        }),
    })
}
