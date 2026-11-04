// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `publish` step: a finished task hands its files to the host.
//!
//! The step digests the files the task names with the harness's digest tool
//! (`require`, so a file that is not there fails the run instead of
//! digesting as absent), records each path with its digest in the trace
//! ([`TraceRecord::Published`]) and goes on. A host that keeps checkpoints
//! takes one when it sees the record: everything before the publication is
//! done, and the files are what the task left. The step is a tool call like
//! any other, so a host that runs its tools elsewhere digests them there.
//!
//! Swedish Embedded AB implements verifiable, resumable agent workflows for
//! its clients. If your team needs expertise in handing finished work from
//! an automated run to the system around it then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};
use sven_harness_spec::{PlanPrimitive, PlanStep, DIGEST_TOOL};
use sven_hsm::{Context, Effect, Reaction, ToolCapability};
use sven_vocab::PublishedFile;

use crate::command_gate::INTERRUPTED_OPERATION;
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

impl HarnessMachine {
    /// `true` when `step` is a `publish` step.
    pub(crate) fn is_publish_step(&self, step: &str) -> bool {
        matches!(
            self.plan().step(step).map(|s| &s.primitive),
            Some(PlanPrimitive::Publish { .. })
        )
    }

    /// The files a publish step names, resolved against the run.
    fn publish_paths(run: &HarnessRunState, step: &PlanStep) -> Vec<String> {
        let PlanPrimitive::Publish { files, .. } = &step.primitive else {
            return Vec::new();
        };
        files
            .iter()
            .map(|template| match run.resolve_template(template) {
                Value::String(text) => text,
                other => other.to_string(),
            })
            .collect()
    }

    /// Starts a publish step: one call of the digest tool, one group per file.
    pub(crate) fn run_publish(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.counts.tool_calls += 1;
        if run.exceeds_tool_budget(self.run.plan()) {
            return self.fail_now(
                run,
                "tool_call_budget",
                "the run exceeded its tool-call budget",
                ctx,
            );
        }
        let groups: serde_json::Map<String, Value> = Self::publish_paths(run, step)
            .into_iter()
            .map(|path| (path.clone(), json!([path])))
            .collect();
        let call_id = self.call_id_for(step);
        let effect = Effect::CallTool {
            call_id,
            name: DIGEST_TOOL.to_string(),
            capability: ToolCapability::ReadFile,
            args: json!({ "groups": groups, "require": true }),
        };
        self.cache_effect(effect.clone());
        self.park(run, step, Waiting::Tool);
        run.inflight = Some(Inflight {
            step: step.id.clone(),
            call_id: Some(call_id),
        });
        run.store(ctx);
        Reaction::Transition {
            target: HarnessState::WaitingForTool,
            effects: vec![effect],
            rationale: format!("publish step `{}` digests the task's files", step.id),
        }
    }

    /// The digest tool answered, or failed: record the publication and go
    /// on, or fail the run naming what is missing.
    pub(crate) fn publish_settled(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        report: Result<&Value, &str>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(step) = self.plan().step(step_id).cloned() else {
            return self.fail_now(run, "unknown_step", "publish step vanished", ctx);
        };
        let PlanPrimitive::Publish { task, then, .. } = &step.primitive else {
            return self.fail_now(run, "internal", "publish result for another step", ctx);
        };
        // A digest whose answer never arrived proved nothing: ask again.
        if report == Err(INTERRUPTED_OPERATION) {
            run.counts.tool_calls = run.counts.tool_calls.saturating_sub(1);
            return self.run_publish(run, &step, ctx);
        }
        let answer = match report {
            Ok(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
            Ok(other) => Some(other.clone()),
            Err(error) => {
                return self.fail_now(
                    run,
                    "publish_failed",
                    &format!("task `{task}` cannot publish its files: {error}"),
                    ctx,
                )
            }
        };
        let paths = Self::publish_paths(run, &step);
        let files: Option<Vec<PublishedFile>> = answer.as_ref().and_then(|answer| {
            paths
                .iter()
                .map(|path| {
                    answer
                        .get(path)
                        .and_then(Value::as_str)
                        .map(|digest| PublishedFile {
                            path: path.clone(),
                            digest: digest.to_string(),
                        })
                })
                .collect()
        });
        let Some(files) = files else {
            return self.fail_now(
                run,
                "publish_failed",
                &format!("the digest tool's answer for task `{task}` does not name every file"),
                ctx,
            );
        };
        run.trace.push(TraceRecord::Published {
            step: step.id.clone(),
            task: task.clone(),
            files,
        });
        run.awaiting = None;
        run.inflight = None;
        let then = then.clone();
        self.walk_to(run, &then, ctx)
    }
}
