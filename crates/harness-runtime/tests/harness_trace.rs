// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness execution trace's spec: a run records what it did as
//! structured records inside its own run state — the step an operation
//! belonged to, the operation id that correlated it, the children a parent
//! started, and the failures with their source step. The trace travels in
//! the run state, so a checkpoint carries its run's history with it.

mod common;

use common::{child_completed, drive, start, turn};
use sven_harness_runtime::{step_call_id, HarnessState, TraceRecord, RUN_STATE_FACT};
use sven_hsm::{Effect, Event};

const READER: &str = r#"
api_version: "2"
name: reader
entry: fetch
capabilities: [ExecuteShell]
limits: { max_steps: 50, max_tool_calls: 5 }
steps:
  fetch:
    gate:
      steps: [{ name: cat, argv: [cat, notes.txt] }]
      then: out
      on_fail: bad
  out:
    return: { value: "{{ steps.fetch.stdout }}" }
  bad:
    fail: { code: gate_failed }
"#;

/// The gate tool's report of a command that exited 0.
fn report(stdout: &str) -> serde_json::Value {
    serde_json::json!({ "exit_code": 0, "timed_out": false, "error": null,
                        "stdout": stdout, "stderr": "" })
    .to_string()
    .into()
}

#[test]
fn a_completed_step_records_itself_and_its_operation_id() {
    let (mut m, mut ctx, mut state) = start(READER, serde_json::json!({}));
    let expected = step_call_id("reader", "fetch");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: expected,
            observation: report("the notes"),
        },
    );
    for _ in 0..8 {
        if state == HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, HarnessState::Completed);
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::StepCompleted { step, call_id: Some(id) }
                if step.as_str() == "fetch" && *id == expected
        )),
        "the gate step's completion names the step and its call id: {trace:?}"
    );
    let _ = ctx.fact(RUN_STATE_FACT).expect("the run state exists");
}

#[test]
fn a_model_turn_records_itself_without_an_operation_id() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("the predict step emitted its turn");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );
    for _ in 0..8 {
        if state == HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, HarnessState::Completed);
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::StepCompleted { step, call_id: None }
                if step.as_str() == "ask"
        )),
        "a model turn stores no operation id: {trace:?}"
    );
    assert!(
        trace
            .iter()
            .any(|r| matches!(r, TraceRecord::Outcome { .. })),
        "the trace ends with the run's outcome: {trace:?}"
    );
}

const GREET: &str = r#"
api_version: "2"
name: greet
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Say hello to {{ inputs.name }}"
      store_as: greeting
      then: out
  out:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

const DELEGATOR: &str = r#"
api_version: "2"
name: delegator
entry: ask
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 2 }
steps:
  ask:
    delegate:
      target: helper
      input: { task: "dig" }
      then: done
  done:
    return:
      value:
        found: "{{ steps.ask.value }}"
"#;

#[test]
fn a_delegating_run_records_the_children_it_started_and_completed() {
    let (mut m, mut ctx, mut state) = start(DELEGATOR, serde_json::json!({}));
    let child = match m.step_effect().expect("the delegate step spawned a child") {
        Effect::InstantiateSubmachine { machine, .. } => machine,
        other => panic!("a delegate spawns a child, got {other:?}"),
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&child, serde_json::json!({ "found": "spade" })),
    );
    let trace = m.trace(&ctx);
    let child_id = child.as_uuid().to_string();
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::ChildStarted { step, child }
                if step.as_str() == "ask" && *child == child_id
        )),
        "the parent's trace names the child machine: {trace:?}"
    );
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::ChildCompleted { step, child, ok: true }
                if step.as_str() == "ask" && *child == child_id
        )),
        "the child's completion is recorded under the parent's step: {trace:?}"
    );
}

#[test]
fn a_failed_step_records_the_step_and_operation_it_failed_on() {
    let (mut m, mut ctx, mut state) = start(DELEGATOR, serde_json::json!({}));
    let child = match m.step_effect().expect("the delegate step spawned a child") {
        Effect::InstantiateSubmachine { machine, .. } => machine,
        other => panic!("a delegate spawns a child, got {other:?}"),
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::EffectFailed {
            kind: sven_hsm::EffectKind::InstantiateSubmachine,
            error: "no child spawner is configured".into(),
        },
    );
    assert_eq!(state, HarnessState::Failed);
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::Failed { step, code, operation: Some(op), .. }
                if step.as_str() == "ask"
                    && code.as_str() == "side_effect_failed"
                    && *op == child.as_uuid().to_string()
        )),
        "the failure names its source step and operation id: {trace:?}"
    );
}
