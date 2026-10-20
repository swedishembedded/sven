// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What lowering knows while it works: the document, the declared inputs, the
//! tasks with structured output and the command gates, and how to turn a
//! gate or a check of the document into one the check lowering can emit.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use super::checks::{CommandRun, Kind, Resolved};
use super::rewrite::{rewrite_text, Scope};
use super::spec::{Check, CommandGate, GateDef, TaskSpec, WorkflowSpec};
use super::validate::split_metric;
use crate::error::HarnessError;
use crate::spec::DEFAULT_GATE_TIMEOUT_SECS;

pub(super) struct Context<'a> {
    pub(super) w: &'a WorkflowSpec,
    pub(super) inputs: Option<BTreeSet<String>>,
    /// Tasks with a `schema`, whose output fields templates can read.
    pub(super) structured: Vec<String>,
    /// Command gates, whose latest result templates can read.
    pub(super) command_gates: Vec<String>,
    /// The authors of corrections, whose messages the accept gates read.
    pub(super) authors: Vec<String>,
}

/// What a task's templates may read besides inputs and earlier outputs.
#[derive(Clone, Copy, Default)]
pub(super) struct LoopVars {
    /// `item`, `item_id` and `item_index`: inside a `for_each`.
    pub(super) item: bool,
    /// `iteration`: inside an `until`.
    pub(super) iteration: bool,
}

impl LoopVars {
    pub(super) fn of(task: &TaskSpec) -> Self {
        Self {
            item: task.for_each.is_some(),
            iteration: task.until.is_some(),
        }
    }
}

impl Context<'_> {
    pub(super) fn scope<'s>(
        &'s self,
        place: String,
        tasks: &'s [String],
        vars: LoopVars,
    ) -> Scope<'s> {
        Scope::new(place, self.inputs.as_ref(), tasks)
            .in_loop(vars.item, vars.iteration)
            .with_structured(&self.structured)
            .with_gates(&self.command_gates)
    }

    pub(super) fn command_run(
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

    pub(super) fn gate(
        &self,
        name: &str,
        vars: LoopVars,
        tasks: &[String],
    ) -> Result<Resolved, HarnessError> {
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

    pub(super) fn metric(
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

    pub(super) fn task_checks(
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
pub(super) fn plain(path: &str) -> String {
    path.replace("{{", "(").replace("}}", ")")
}
