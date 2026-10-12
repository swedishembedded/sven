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

/// A child machine's completion event, correlated by its machine id.
fn completed(machine: &str, result: serde_json::Value) -> Event {
    Event::Internal(sven_hsm::event::InternalEvent::SubmachineCompleted {
        machine: machine.to_string(),
        result,
    })
}

const READER: &str = r#"
api_version: "1"
name: reader
entry: fetch
limits: { max_steps: 50, max_tool_calls: 5 }
steps:
  fetch:
    tool:
      name: read_file
      args: { path: "notes.txt" }
      then: out
  out:
    return: { value: "{{ steps.fetch.value }}" }
"#;

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
            observation: serde_json::json!("the notes"),
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
        "the tool step's completion names the step and its call id: {trace:?}"
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
api_version: "1"
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

#[test]
fn a_failed_step_records_the_step_and_operation_it_failed_on() {
    let (mut m, mut ctx, mut state) = start(READER, serde_json::json!({}));
    let expected = step_call_id("reader", "fetch");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: expected,
            error: "file missing".into(),
        },
    );
    assert_eq!(state, HarnessState::Failed);
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::Failed { step, code, operation: Some(op), .. }
                if step.as_str() == "fetch"
                    && code.as_str() == "side_effect_failed"
                    && *op == expected.as_uuid().to_string()
        )),
        "the failure names its source step and operation id: {trace:?}"
    );
}

const DELEGATOR: &str = r#"
api_version: "1"
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

const FANOUT: &str = r#"
api_version: "1"
name: fanout
entry: split
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3, max_parallel: 4 }
steps:
  split:
    parallel:
      branches: [left, right]
      join: merge
  left:
    assign: { set: { kind: "left" }, then: left_out }
  left_out:
    return: { value: { side: "left" } }
  right:
    assign: { set: { kind: "right" }, then: right_out }
  right_out:
    return: { value: { side: "right" } }
  merge:
    return:
      value:
        results: "{{ steps.split.value }}"
"#;

#[test]
fn a_fan_out_records_its_branches_and_its_join() {
    let (mut m, mut ctx, mut state) = start(FANOUT, serde_json::json!({}));
    // The fan-out's children: one per branch, in declared order (the open
    // join record is the correlation table).
    let machines: Vec<String> = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v.get("join").map(|j| j["machines"].clone()))
        .and_then(|v| serde_json::from_value::<Vec<(String, String)>>(v).ok())
        .expect("the join record names the children")
        .into_iter()
        .map(|(_, machine)| machine)
        .collect();
    assert_eq!(machines.len(), 2);

    // Completing out of order: the join record still names each child.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        completed(&machines[1], serde_json::json!({ "side": "right" })),
    );
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        completed(&machines[0], serde_json::json!({ "side": "left" })),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::FanOut { step, branches }
                if step.as_str() == "split"
                    && *branches == ["left".to_string(), "right".to_string()]
        )),
        "the fan-out names its branches in declared order: {trace:?}"
    );
    for (child, ok) in [(machines[0].as_str(), true), (machines[1].as_str(), true)] {
        assert!(
            trace.iter().any(|r| matches!(
                r,
                TraceRecord::ChildCompleted { child: c, ok: o, .. }
                    if c.as_str() == child && *o == ok
            )),
            "each branch child's completion is recorded: {trace:?}"
        );
    }
    assert!(
        trace
            .iter()
            .any(|r| matches!(r, TraceRecord::Joined { step, results } if step.as_str() == "split" && *results == 2)),
        "the join records how many results settled: {trace:?}"
    );
}

#[test]
fn a_retry_records_each_attempt_it_makes() {
    let yaml = r#"
api_version: "1"
name: flaky
entry: attempt
limits: { max_steps: 100, max_tool_calls: 10 }
steps:
  attempt:
    retry:
      attempts: 2
      body: probe
      then: ok
      on_exhausted: give_up
  probe:
    tool:
      name: read_file
      args: { path: "flaky-probe" }
      then: check
  check:
    branch:
      on: "!steps.probe.ok"
      when_true: attempt
      when_false: ok
  ok:
    return: { value: "made it" }
  give_up:
    fail: { code: "unreachable", message: "the probe never succeeded" }
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let call = |n: u64| step_call_id("flaky", &format!("probe-{n}"));
    let _ = m.step_effect().expect("the body step emitted its call");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: call(1),
            error: "file missing".into(),
        },
    );
    assert_eq!(
        state,
        HarnessState::WaitingForTool,
        "attempt 2 is in flight"
    );
    let trace = m.trace(&ctx);
    assert!(
        trace.iter().any(|r| matches!(
            r,
            TraceRecord::RetryAttempt { step, attempt: 2 }
                if step.as_str() == "probe"
        )),
        "the retry records the attempt it is about to make: {trace:?}"
    );
}
