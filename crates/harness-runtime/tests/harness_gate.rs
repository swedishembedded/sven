// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a `gate` step lets a command's exit status, and nothing else,
//! decide whether a task succeeded; a delegated child's failure cannot be
//! turned into a parent's success; and the model can neither run a gate nor
//! make one pass. Driven at the kernel boundary like the other machine tests.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{child_completed, drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessState, RUN_STATE_FACT};
use sven_hsm::{Context, Effect, Event, ProposedToolCall, ToolCallId, ToolCapability};

const GATED: &str = r#"
api_version: "2"
name: gated
inputs: { target: string }
execution: { limits: { model_turns: 10 } }
gates:
  test: { command: [sh, check.sh, "{{ inputs.target }}"], timeout: 30 }
workflow:
  - name: fix
    task: Fix {{ inputs.target }}.
    verify: [test]
    repair: { attempts: 1 }
"#;

const THREAD: &str = "harness.gated";

fn begin() -> (HarnessMachine, Context, HarnessState) {
    start(GATED, json!({ "target": "lib" }))
}

/// Finishes the agent turn with `text`; returns the effects that follow.
fn finish_turn(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    state: &mut HarnessState,
    text: &str,
) -> Vec<Effect> {
    drive(m, ctx, state, turn(THREAD, text))
}

fn gate_call(effects: &[Effect]) -> (ToolCallId, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallTool {
                call_id,
                name,
                capability,
                args,
            } => {
                assert_eq!(name, "shell_gate");
                assert_eq!(*capability, ToolCapability::ExecuteShell);
                Some((*call_id, args.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a gate call in {effects:?}"))
}

fn report(exit_code: i64, stdout: &str, stderr: &str) -> Value {
    json!({
        "exit_code": exit_code, "timed_out": false, "error": null,
        "stdout": stdout, "stderr": stderr, "duration_ms": 5,
    })
    .to_string()
    .into()
}

fn answer(call_id: ToolCallId, observation: Value) -> Event {
    Event::ToolSucceeded {
        call_id,
        observation,
    }
}

fn run_state(ctx: &Context) -> Value {
    ctx.fact(RUN_STATE_FACT).cloned().expect("run state")
}

#[test]
fn a_gate_calls_the_gate_tool_with_a_resolved_argv() {
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
    let (_, args) = gate_call(&fx);
    assert_eq!(args["steps"][0]["argv"], json!(["sh", "check.sh", "lib"]));
    assert_eq!(args["timeout_secs"], 30);
    assert_eq!(state, HarnessState::WaitingForTool);
}

#[test]
fn gate_command_exit_code_is_authoritative() {
    // The command prints that everything passed and exits 1: the gate fails.
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(
        &mut m,
        &mut ctx,
        &mut state,
        "all green, I ran the tests myself",
    );
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        answer(call, report(1, "ALL TESTS PASSED", "")),
    );
    // One repair turn is earned, not a success.
    assert_ne!(state, HarnessState::Completed);
    let gate_result = &run_state(&ctx)["steps"]["fix__v0_test_a0"]["value"];
    assert_eq!(gate_result["passed"], false);
    assert_eq!(gate_result["kind"], "failed");
    assert_eq!(gate_result["exit_code"], 1);
    assert_eq!(gate_result["stdout"], "ALL TESTS PASSED");
}

#[test]
fn a_zero_exit_passes_and_completes_the_run() {
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        answer(call, report(0, "ok", "")),
    );
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(m.outcome().expect("outcome")["status"], "completed");
}

#[test]
fn a_failed_gate_earns_a_repair_turn_that_sees_the_output_and_then_the_end() {
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
    let (call, _) = gate_call(&fx);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        answer(call, report(2, "", "assertion failed: left == right")),
    );
    let prompt = fx
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => request["instruction"].as_str().map(str::to_string),
            _ => None,
        })
        .expect("the repair turn is a model call");
    assert!(
        prompt.contains("assertion failed: left == right"),
        "{prompt}"
    );
    assert!(prompt.contains("status 2"), "{prompt}");

    // The repair did not help: the second verification fails and the run ends.
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "fixed it");
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        answer(call, report(2, "", "still red")),
    );
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["code"], "gate_failed");
    assert!(outcome["message"]
        .as_str()
        .is_some_and(|m| m.contains("still red") || m.contains("status 2")));
}

#[test]
fn every_way_a_gate_cannot_be_shown_to_pass_is_a_failure() {
    let cases: [(&str, Value, &str); 6] = [
        (
            "timeout",
            json!({"exit_code": null, "timed_out": true, "error": null, "stdout": "", "stderr": ""}),
            "timeout",
        ),
        (
            "spawn error",
            json!({"exit_code": null, "timed_out": false, "error": "no such file", "stdout": "", "stderr": ""}),
            "exec_error",
        ),
        (
            "no exit status",
            json!({"exit_code": null, "timed_out": false, "error": null}),
            "exec_error",
        ),
        (
            "zero exit with an error",
            json!({"exit_code": 0, "timed_out": false, "error": "killed"}),
            "exec_error",
        ),
        (
            "zero exit but timed out",
            json!({"exit_code": 0, "timed_out": true, "error": null}),
            "timeout",
        ),
        (
            "not json",
            Value::String("tests passed".into()),
            "exec_error",
        ),
    ];
    for (what, report, kind) in cases {
        // A report is JSON text; the last case is plain text, not a report.
        let observation = if report.is_string() {
            report
        } else {
            report.to_string().into()
        };
        let (mut m, mut ctx, mut state) = begin();
        let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
        let (call, _) = gate_call(&fx);
        let _ = drive(&mut m, &mut ctx, &mut state, answer(call, observation));
        assert_ne!(state, HarnessState::Completed, "{what} must not pass");
        let gate = &run_state(&ctx)["steps"]["fix__v0_test_a0"]["value"];
        assert_eq!(gate["passed"], false, "{what}");
        assert_eq!(gate["kind"], kind, "{what}");
    }
}

#[test]
fn a_gate_tool_that_fails_outright_is_a_failed_gate() {
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: call,
            error: "unknown tool".into(),
        },
    );
    assert_ne!(state, HarnessState::Completed);
    let gate = &run_state(&ctx)["steps"]["fix__v0_test_a0"]["value"];
    assert_eq!(gate["kind"], "exec_error");
    assert!(gate["detail"]
        .as_str()
        .is_some_and(|d| d.contains("unknown tool")));
}

#[test]
fn the_model_cannot_run_a_gate() {
    // An open agent step (no tool list) is admitted any session tool except
    // the gate tool, which belongs to the workflow.
    let (mut m, mut ctx, mut state) = begin();
    let call_id = ToolCallId::new();
    let proposal = Event::LlmTurnComplete {
        thread: THREAD.into(),
        text: String::new(),
        tool_calls: vec![ProposedToolCall {
            call_id,
            name: "shell_gate".into(),
            capability: ToolCapability::ExecuteShell,
            args: json!({ "argv": ["true"] }),
        }],
    };
    let fx = drive(&mut m, &mut ctx, &mut state, proposal);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::CallTool { .. })),
        "the gate tool must never be called on the agent's behalf: {fx:?}"
    );
    // The model is told why on its next turn.
    let told = fx.iter().any(|e| match e {
        Effect::CallLlm { request } => request
            .to_string()
            .contains("reserved to the workflow's own steps"),
        _ => false,
    });
    assert!(told, "the refusal reaches the model: {fx:?}");
}

const DELEGATING: &str = r#"
api_version: "2"
name: parent
entry: go
limits: { max_steps: 10 }
capabilities: [SpawnChild]
steps:
  go: { delegate: { target: helper, input: {}, store_as: child, then: after } }
  after: { return: { value: reached } }
"#;

fn parent() -> (HarnessMachine, Context, HarnessState) {
    let plan = common::plan(DELEGATING).expect("compiles");
    let mut m = HarnessMachine::new(HarnessRunData::from_parts(plan, json!({})));
    let mut ctx = Context::new();
    let mut state = sven_hsm::Machine::initial(&m);
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state)
}

#[test]
fn child_failure_does_not_succeed_parent() {
    let (mut m, mut ctx, mut state) = parent();
    let Some(Effect::InstantiateSubmachine { machine, .. }) = m.step_effect() else {
        panic!("delegate emits a child")
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&machine, json!({ "ok": false, "message": "child gave up" })),
    );
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["code"], "child_failed");
    assert_eq!(outcome["message"], "child gave up");
}

#[test]
fn a_child_that_succeeds_still_continues_the_parent() {
    let (mut m, mut ctx, mut state) = parent();
    let Some(Effect::InstantiateSubmachine { machine, .. }) = m.step_effect() else {
        panic!("delegate emits a child")
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&machine, json!({ "ok": true })),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
}

#[test]
fn resume_does_not_trust_interrupted_side_effect() {
    // The run was suspended while the gate's command ran: no report ever
    // came back. That is neither a pass nor a failure of the check, so the
    // gate runs again and no repair attempt is spent on it.
    let (mut m, mut ctx, mut state) = begin();
    let fx = finish_turn(&mut m, &mut ctx, &mut state, "done");
    let (call, _) = gate_call(&fx);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: call,
            error: sven_harness_runtime::INTERRUPTED_OPERATION.to_string(),
        },
    );
    let (again, args) = gate_call(&fx);
    assert_eq!(again, call, "the same step asks again");
    assert_eq!(args["steps"][0]["argv"], json!(["sh", "check.sh", "lib"]));
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::CallLlm { .. })),
        "no repair turn"
    );
    assert_eq!(state, HarnessState::WaitingForTool);
    // The answer to the second run decides, and only it.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        answer(call, report(0, "", "")),
    );
    assert_eq!(state, HarnessState::Completed);
}
