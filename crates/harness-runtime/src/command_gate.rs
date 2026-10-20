// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `gate` step: a command whose exit status decides.
//!
//! A gate calls the harness-only tool [`GATE_TOOL`] with the command's argv
//! and reads the tool's report. The machine, not the model and not the
//! command's own output, decides whether the gate passed: the report must say
//! the command ran, did not time out, and exited with status 0. Everything
//! else fails the gate, with a kind that says why (`failed`, `timeout`,
//! `exec_error`), and a report the machine cannot read is a failure too. The
//! typed result is stored under the step so repair prompts, traces and
//! checkpoints can read the exit status and the captured output.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};
use sven_harness_spec::{PlanPrimitive, PlanStep, Template, GATE_TOOL};
use sven_hsm::{Context, Effect, Reaction, ToolCapability};

use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

/// The error a resumed run settles an in-flight operation with: the
/// operation did not complete, so nothing is known about it. The SDK builds
/// the settling event from this text and the machine recognises it.
pub const INTERRUPTED_OPERATION: &str =
    "the run was suspended with this operation in flight; it did not complete";

/// The capability a gate's command exercises.
pub(super) const GATE_CAPABILITY: ToolCapability = ToolCapability::ExecuteShell;

/// The arguments of the gate tool call for a command: the argv with its
/// templates resolved to strings, the timeout and the working directory.
pub(super) fn gate_args(
    run: &HarnessRunState,
    command: &[Template],
    timeout_secs: u64,
    cwd: Option<&str>,
    stdin: Option<&Template>,
) -> Value {
    let argv: Vec<String> = command
        .iter()
        .map(|arg| match run.resolve_template(arg) {
            Value::String(text) => text,
            other => other.to_string(),
        })
        .collect();
    let mut args = json!({ "argv": argv, "timeout_secs": timeout_secs });
    if let Some(cwd) = cwd {
        args["cwd"] = json!(cwd);
    }
    if let Some(stdin) = stdin {
        args["stdin"] = match run.resolve_template(stdin) {
            Value::String(text) => json!(text),
            other => json!(other.to_string()),
        };
    }
    args
}

/// What one gate evaluation found, as stored under the step.
pub(super) struct GateResult {
    passed: bool,
    kind: &'static str,
    exit_code: Option<i64>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    detail: String,
    /// The standard output parsed as JSON, when it is a JSON document: a
    /// check that reports its findings as data is read through this.
    json: Value,
}

impl GateResult {
    fn exec_error(detail: String) -> Self {
        Self {
            passed: false,
            kind: "exec_error",
            exit_code: None,
            timed_out: false,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            detail,
            json: Value::Null,
        }
    }

    /// Reads the tool's report. The report is JSON text (or an object) with
    /// `exit_code`, `timed_out`, `error`, `stdout`, `stderr`, `duration_ms`.
    pub(super) fn from_report(observation: &Value, timeout_secs: u64) -> Self {
        let report = match observation {
            Value::String(text) => serde_json::from_str::<Value>(text).ok(),
            other => Some(other.clone()),
        };
        let Some(Value::Object(report)) = report else {
            return Self::exec_error("the gate tool returned a report that is not JSON".into());
        };
        let text = |key: &str| {
            report
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let exit_code = report.get("exit_code").and_then(Value::as_i64);
        let timed_out = report
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let error = text("error");
        let (kind, detail) = if !error.is_empty() {
            (
                "exec_error",
                format!("the command could not be run: {error}"),
            )
        } else if timed_out {
            (
                "timeout",
                format!("the command timed out after {timeout_secs} s"),
            )
        } else {
            match exit_code {
                Some(0) => ("passed", "the command exited with status 0".to_string()),
                Some(code) => ("failed", format!("the command exited with status {code}")),
                None => (
                    "exec_error",
                    "the command ended without an exit status".to_string(),
                ),
            }
        };
        let stdout = text("stdout");
        let json = serde_json::from_str::<Value>(stdout.trim())
            .ok()
            .filter(|v| v.is_object() || v.is_array())
            .unwrap_or(Value::Null);
        Self {
            passed: kind == "passed",
            json,
            kind,
            exit_code,
            timed_out,
            stdout,
            stderr: text("stderr"),
            duration_ms: report
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            detail,
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "passed": self.passed,
            "kind": self.kind,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "duration_ms": self.duration_ms,
            "detail": self.detail,
            "json": self.json,
        })
    }
}

impl HarnessMachine {
    /// `true` when `error` says a suspension interrupted the operation and
    /// the plan is a workflow: a workflow's model turns and verifications
    /// start again on resume instead of failing the run. (A version 1
    /// document keeps the retry policy it declares.)
    pub(super) fn restarts_interrupted(&self, error: &str) -> bool {
        error == INTERRUPTED_OPERATION
            && self.run.plan().api_version == sven_harness_spec::WORKFLOW_API_VERSION
    }

    /// Starts the interrupted step again from its beginning. The aborted
    /// attempt is not counted against the step or turn budgets: it never
    /// finished, and a resume should not make a run run out of them.
    pub(super) fn restart_interrupted(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.counts.steps = run.counts.steps.saturating_sub(1);
        if matches!(
            self.run.plan().step(step_id).map(|s| &s.primitive),
            Some(PlanPrimitive::Predict { .. } | PlanPrimitive::Agent { .. })
        ) {
            run.counts.model_turns = run.counts.model_turns.saturating_sub(1);
        }
        run.agent = None;
        run.awaiting = None;
        run.inflight = None;
        self.walk_to(run, step_id, ctx)
    }

    /// Starts a gate step: one call of the gate tool, parked in
    /// [`HarnessState::WaitingForTool`].
    pub(super) fn run_gate(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let PlanPrimitive::Gate {
            command,
            timeout_secs,
            cwd,
            stdin,
            ..
        } = &step.primitive
        else {
            return self.fail_now(run, "internal", "gate dispatch reached another step", ctx);
        };
        run.counts.tool_calls += 1;
        if run.exceeds_tool_budget(self.run.plan()) {
            return self.fail_now(
                run,
                "tool_call_budget",
                "the run exceeded its tool-call budget",
                ctx,
            );
        }
        let call_id = self.call_id_for(step, run);
        let effect = Effect::CallTool {
            call_id,
            name: GATE_TOOL.to_string(),
            capability: GATE_CAPABILITY,
            args: gate_args(run, command, *timeout_secs, cwd.as_deref(), stdin.as_ref()),
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
            rationale: format!("gate step `{}` runs its command", step.id),
        }
    }

    /// The gate tool answered, or failed: judge the report and continue at
    /// `then` on a pass, at `on_fail` on anything else. A tool that failed
    /// outright (it was missing, refused or crashed) is a failed gate.
    pub(super) fn gate_settled(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        report: Result<&Value, &str>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(PlanPrimitive::Gate {
            timeout_secs,
            store_as,
            then,
            on_fail,
            ..
        }) = self.run.plan().step(step_id).map(|s| s.primitive.clone())
        else {
            return self.fail_now(run, "internal", "gate result for a non-gate step", ctx);
        };
        // A gate whose answer never arrived proved nothing, either way: it is
        // run again, without costing a repair attempt or a step. (Re-running
        // a verification is safe; the command's earlier effects, if any, are
        // the command's own concern.)
        if report == Err(INTERRUPTED_OPERATION) {
            let Some(step) = self.run.plan().step(step_id).cloned() else {
                return self.fail_now(run, "unknown_step", "interrupted gate vanished", ctx);
            };
            run.counts.tool_calls = run.counts.tool_calls.saturating_sub(1);
            return self.run_gate(run, &step, ctx);
        }
        let result = match report {
            Ok(observation) => GateResult::from_report(observation, timeout_secs),
            Err(error) => GateResult::exec_error(format!("the gate tool failed: {error}")),
        };
        run.trace.push(TraceRecord::GateEvaluated {
            step: step_id.to_string(),
            passed: result.passed,
            kind: result.kind.to_string(),
            exit_code: result.exit_code,
        });
        let next = if result.passed { then } else { on_fail };
        let value = result.to_value();
        run.store_step_value(step_id, value, store_as.as_deref());
        self.settle_step(run, step_id, &next, ctx)
    }
}
