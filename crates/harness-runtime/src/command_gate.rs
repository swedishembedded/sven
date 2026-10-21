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

use crate::host_values::{RUN_FACT, TRAJECTORY_FILE};
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

/// The error a resumed run settles an in-flight operation with: the
/// operation did not complete, so nothing is known about it. The SDK builds
/// the settling event from this text and the machine recognises it.
pub const INTERRUPTED_OPERATION: &str =
    "the run was suspended with this operation in flight; it did not complete";

/// The environment variable a command gate finds the run's scratchpad in.
const SCRATCHPAD_ENV: &str = "SVEN_SCRATCHPAD";

/// The capability a gate's command exercises.
pub(super) const GATE_CAPABILITY: ToolCapability = ToolCapability::ExecuteShell;

/// The arguments of the gate tool call: the steps with their argv templates
/// resolved to strings, the variables, the timeout and the working directory.
pub(super) fn gate_args(run: &HarnessRunState, gate: &PlanPrimitive) -> Value {
    let PlanPrimitive::Gate {
        steps,
        env,
        timeout_secs,
        cwd,
        cwd_input,
        output_bytes,
        stdin,
        ..
    } = gate
    else {
        return Value::Null;
    };
    let (timeout_secs, cwd, cwd_input, stdin) = (
        *timeout_secs,
        cwd.as_deref(),
        cwd_input.as_deref(),
        stdin.as_ref(),
    );
    let text = |template: &Template| match run.resolve_template(template) {
        Value::String(text) => text,
        other => other.to_string(),
    };
    let steps: Vec<Value> = steps
        .iter()
        .map(|step| {
            let argv: Vec<String> = step.argv.iter().map(text).collect();
            json!({ "name": step.name, "argv": argv })
        })
        .collect();
    let mut args = json!({ "steps": steps, "timeout_secs": timeout_secs });
    if let Some(bytes) = output_bytes {
        args["tail_bytes"] = json!(bytes);
    }
    if let Some(cwd) = cwd {
        args["cwd"] = json!(cwd);
    }
    if let Some(dir) = cwd_input
        .and_then(|name| run.inputs.get(name))
        .and_then(Value::as_str)
    {
        args["trusted_cwd"] = json!(dir);
    }
    let host = run.run_values();
    if gate_templates(gate).any(reads_trajectory_file) {
        if let Some(file) = host.trajectory_file {
            args["trajectory_file"] = json!(file);
        }
    }
    // The host's variable is set last: a gate cannot point itself elsewhere.
    let mut variables: serde_json::Map<String, Value> = env
        .iter()
        .map(|(name, value)| (name.clone(), json!(text(value))))
        .collect();
    if let Some(dir) = host.scratchpad {
        variables.insert(SCRATCHPAD_ENV.to_string(), json!(dir));
    }
    if !variables.is_empty() {
        args["env"] = Value::Object(variables);
    }
    if let Some(stdin) = stdin {
        args["stdin"] = match run.resolve_template(stdin) {
            Value::String(text) => json!(text),
            other => json!(other.to_string()),
        };
    }
    args
}

/// How many times an empty model reply is asked again before the run fails.
const MAX_EMPTY_REPLY_RETRIES: u32 = 2;
/// How many times a turn is asked again after a failure that may pass.
const MAX_TRANSIENT_RETRIES: u32 = 4;

/// What went wrong with a model turn that is worth asking again.
#[derive(Clone, Copy)]
pub(super) enum TurnTrouble<'a> {
    /// No text and no tool call.
    Empty,
    /// The server failed in a way that may pass, with its error.
    Transient(&'a str),
}

/// `true` when a template reads `{{ run.trajectory_file }}`.
pub(super) fn reads_trajectory_file(template: &Template) -> bool {
    template.paths().iter().any(|path| {
        matches!(path.segments(), [facts, run, file]
            if facts == "facts" && run == RUN_FACT && file == TRAJECTORY_FILE)
    })
}

/// Every template a gate's command is made of: its arguments, its variables
/// and its standard input.
fn gate_templates(gate: &PlanPrimitive) -> impl Iterator<Item = &Template> {
    let (steps, env, stdin) = match gate {
        PlanPrimitive::Gate {
            steps, env, stdin, ..
        } => (steps.as_slice(), Some(env), stdin.as_ref()),
        _ => (&[][..], None, None),
    };
    steps
        .iter()
        .flat_map(|step| step.argv.iter())
        .chain(env.into_iter().flat_map(|env| env.values()))
        .chain(stdin)
}

/// `true` when a gate of `plan` reads `{{ run.trajectory_file }}`: the host
/// then has to provide the file.
#[must_use]
pub fn plan_reads_trajectory(plan: &sven_harness_spec::HarnessPlan) -> bool {
    plan.steps
        .values()
        .any(|step| gate_templates(&step.primitive).any(reads_trajectory_file))
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
    /// The step that ended the gate when it did not pass.
    failed_step: Option<String>,
    /// Each step that ran: its name, exit status and duration.
    steps: Value,
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
            failed_step: None,
            steps: json!([]),
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
        let failed_step = report
            .get("failed_step")
            .and_then(Value::as_str)
            .map(str::to_string);
        let detail = match &failed_step {
            Some(step) if kind != "passed" => format!("{detail} (step `{step}`)"),
            _ => detail,
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
            failed_step,
            steps: report.get("steps").cloned().unwrap_or_else(|| json!([])),
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
            "failed_step": self.failed_step,
            "steps": self.steps,
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
        run.reconcile_pending = true;
        self.walk_to(run, step_id, ctx)
    }

    /// The model answered with nothing, or the server failed in a way that
    /// may pass: ask the turn again, a bounded number of times and after a
    /// pause that grows, each ask on the trace. When the asks are spent the run
    /// fails with a code that says what happened (`empty_reply`,
    /// `model_unavailable`), not that some side effect failed. A retry costs
    /// neither a step nor a model turn.
    pub(super) fn retry_turn(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        why: TurnTrouble<'_>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let (reason, max, base_ms) = match why {
            TurnTrouble::Empty => ("empty_reply", MAX_EMPTY_REPLY_RETRIES, 2_000),
            TurnTrouble::Transient(_) => ("transient", MAX_TRANSIENT_RETRIES, 5_000),
        };
        if run.turn_retries >= max {
            let times = run.turn_retries + 1;
            run.turn_retries = 0;
            run.retry_delay_ms = 0;
            let (code, message) = match why {
                TurnTrouble::Empty => (
                    "empty_reply",
                    format!(
                        "the model answered {times} times with nothing (no text and no tool \
                         call) at step `{step_id}`"
                    ),
                ),
                TurnTrouble::Transient(error) => (
                    "model_unavailable",
                    format!("the model server failed {times} times at step `{step_id}`: {error}"),
                ),
            };
            return self.fail_now(run, code, &message, ctx);
        }
        run.turn_retries += 1;
        run.retry_delay_ms = base_ms << (run.turn_retries - 1);
        run.trace.push(TraceRecord::TurnRetried {
            step: step_id.to_string(),
            reason: reason.to_string(),
            attempt: u64::from(run.turn_retries),
        });
        run.counts.steps = run.counts.steps.saturating_sub(1);
        run.counts.model_turns = run.counts.model_turns.saturating_sub(1);
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
        if !matches!(step.primitive, PlanPrimitive::Gate { .. }) {
            return self.fail_now(run, "internal", "gate dispatch reached another step", ctx);
        }
        run.counts.tool_calls += 1;
        if run.exceeds_tool_budget(self.run.plan()) {
            return self.fail_now(
                run,
                "tool_call_budget",
                "the run exceeded its tool-call budget",
                ctx,
            );
        }
        if gate_templates(&step.primitive).any(reads_trajectory_file) {
            // The host decides where the trajectory goes (and is the only one
            // who can write it); a host that gave no place cannot run this gate.
            if run.run_values().trajectory_file.is_none() {
                return self.fail_now(
                    run,
                    "trajectory_unavailable",
                    "a gate reads the run's trajectory but this host provides none",
                    ctx,
                );
            }
        }
        let call_id = self.call_id_for(step, run);
        let effect = Effect::CallTool {
            call_id,
            name: GATE_TOOL.to_string(),
            capability: GATE_CAPABILITY,
            args: gate_args(run, &step.primitive),
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
