// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The schema'd `predict` and `delegate` steps' spec: the same
//! kernel-boundary drivers as `harness_machine.rs`, for the steps whose
//! contracts arrived after the core walk.

mod common;

use common::{child_completed, drive, start, turn};
use sven_harness_runtime::{HarnessState, RUN_STATE_FACT};
use sven_hsm::{Effect, Event, MachineId};

const SCHEMA_HARNESS: &str = r#"
api_version: "1"
name: shaped
entry: ask
limits: { max_steps: 20, max_model_turns: 5 }
schemas:
  answer:
    type: object
    properties:
      greeting: { type: string }
    required: [greeting]
steps:
  ask:
    predict:
      prompt: "Say hello to {{ inputs.name }}"
      schema: answer
      store_as: spoken
      then: done
  done:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

#[test]
fn a_predict_step_with_a_schema_constrains_the_turn() {
    let (mut m, mut ctx, mut state) = start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
    let ask = m.step_effect().expect("effect");
    let Effect::CallLlm { request } = &ask else {
        panic!("predict emits CallLlm, got {ask:?}");
    };
    assert_eq!(request["schema_name"], "answer");
    assert_eq!(request["schema"]["type"], "object");
    assert_eq!(request["schema"]["required"][0], "greeting");

    // A conforming JSON answer is parsed and stored as the object.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.shaped", "{\"greeting\": \"hi Ada\"}"),
    );
    assert!(
        fx.is_empty(),
        "storing the answer advances by internal event only"
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("ask"))
            .map(|s| s["value"].clone())),
        Some(serde_json::json!({"greeting": "hi Ada"}))
    );
}

#[test]
fn a_non_json_answer_to_a_schema_turn_fails_the_step() {
    let (mut m, mut ctx, mut state) = start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("effect");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.shaped", "not json"),
    );
    assert_eq!(state, HarnessState::Failed);
    let out = m.outcome().expect("outcome");
    assert_eq!(out["code"], "invalid_answer");
    let _ = fx;
}

const CALLER: &str = r#"
api_version: "1"
name: caller
entry: go
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3 }
steps:
  go:
    delegate:
      target: helper
      input:
        task: "summarize {{ inputs.topic }}"
      store_as: summary
      then: out
  out:
    return:
      value:
        summary: "{{ facts.summary }}"
        verdict: "{{ steps.go.value.ok }}"
"#;

#[test]
fn a_delegate_step_emits_instantiate_submachine_and_parks_waiting_for_child() {
    let (mut m, mut ctx, mut state) = start(CALLER, serde_json::json!({ "topic": "rust" }));
    let ask = m.step_effect().expect("effect");
    let Effect::InstantiateSubmachine {
        machine,
        descriptor,
    } = &ask
    else {
        panic!("delegate emits InstantiateSubmachine, got {ask:?}");
    };
    assert_eq!(descriptor["harness"], "helper");
    assert_eq!(descriptor["input"]["task"], "summarize rust");
    assert_eq!(state, HarnessState::WaitingForChild);

    // The child's completion, correlated by the emitted machine id, is
    // stored under the step and its `store_as`, and the walk follows `then`.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(
            machine,
            serde_json::json!({ "ok": true, "summary": "rust is nice" }),
        ),
    );
    assert!(
        fx.is_empty(),
        "storing the result advances by internal event only"
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("go"))
            .map(|s| s["value"]["summary"].clone())),
        Some(serde_json::json!("rust is nice"))
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT)
            .and_then(|v| v.get("facts").and_then(|f| f.get("summary")).cloned()),
        Some(serde_json::json!({ "ok": true, "summary": "rust is nice" }))
    );
}

#[test]
fn a_delegate_completion_fills_the_output_contract() {
    let (mut m, mut ctx, mut state) = start(CALLER, serde_json::json!({ "topic": "rust" }));
    let Effect::InstantiateSubmachine { machine, .. } = m.step_effect().expect("effect") else {
        panic!("delegate emits InstantiateSubmachine");
    };
    // Settle the child, then tick: the pure tail (`return`) runs and the
    // run completes with the child's result in the output.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(
            &machine,
            serde_json::json!({ "ok": true, "summary": "rust is nice" }),
        ),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        m.outcome().expect("outcome")["summary"],
        serde_json::json!({ "ok": true, "summary": "rust is nice" })
    );
    assert_eq!(
        m.outcome().expect("outcome")["verdict"],
        serde_json::json!(true)
    );
}

#[test]
fn a_delegate_step_ignores_a_completion_from_an_unmatched_machine() {
    let (mut m, mut ctx, mut state) = start(CALLER, serde_json::json!({ "topic": "rust" }));
    let Effect::InstantiateSubmachine { machine, .. } = m.step_effect().expect("effect") else {
        panic!("delegate emits InstantiateSubmachine");
    };
    let other = MachineId::new();
    assert_ne!(other.as_uuid(), machine.as_uuid());
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&other, serde_json::json!({ "ok": false })),
    );
    assert!(fx.is_empty(), "a foreign completion settles nothing");
    assert_eq!(state, HarnessState::WaitingForChild);
    // The awaited child still completes afterwards.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&machine, serde_json::json!({ "ok": true })),
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("go"))
            .map(|s| s["value"]["ok"].clone())),
        Some(serde_json::json!(true))
    );
}

#[test]
fn a_delegate_failure_loops_the_retry_frame_or_fails_the_run() {
    // Without a retry frame: the failure fails the run.
    let (mut m, mut ctx, mut state) = start(CALLER, serde_json::json!({ "topic": "rust" }));
    let _ = m.step_effect().expect("effect");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::EffectFailed {
            kind: sven_hsm::EffectKind::InstantiateSubmachine,
            error: "no child spawner is configured".into(),
        },
    );
    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().expect("outcome")["code"], "side_effect_failed");
    let _ = fx;
}

const BOUNCED_CALLER: &str = r#"
api_version: "1"
name: bounced
entry: guard
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3 }
steps:
  guard:
    retry:
      attempts: 2
      body: go
      then: done
      on_exhausted: give_up
  go:
    delegate:
      target: helper
      input: { task: "x" }
      then: done
  done:
    return: { value: "child said ok" }
  give_up:
    fail: { code: "child_never_ran" }
"#;

#[test]
fn a_delegate_failure_within_a_retry_frame_re_enters_the_body() {
    let (mut m, mut ctx, mut state) = start(BOUNCED_CALLER, serde_json::json!({}));
    let _ = m.step_effect().expect("first attempt emitted");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::EffectFailed {
            kind: sven_hsm::EffectKind::InstantiateSubmachine,
            error: "child run failed".into(),
        },
    );
    // The retry frame loops straight back into the body: a fresh
    // InstantiateSubmachine for the second attempt.
    let again = m.step_effect().expect("second attempt emitted");
    let Effect::InstantiateSubmachine {
        machine: second, ..
    } = &again
    else {
        panic!("retry re-emits InstantiateSubmachine, got {again:?}");
    };
    // The retry's second attempt completes: the run follows the body's
    // `then` out of the retry to the terminal.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(second, serde_json::json!("child said ok")),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        m.outcome().expect("outcome"),
        serde_json::json!("child said ok")
    );
}

const DEEPCALLER: &str = r#"
api_version: "1"
name: deepcaller
entry: go
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 1 }
steps:
  go:
    delegate:
      target: helper
      input: { task: "x" }
      then: out
  out:
    return: { value: "done" }
"#;

#[test]
fn a_delegate_beyond_max_delegate_depth_fails_before_emitting() {
    let (m, ctx, state) = start(DEEPCALLER, serde_json::json!({}));
    // The parent already sits at depth 1 (itself); a delegate with
    // max_delegate_depth: 1 has no headroom, so the walk fails the run
    // without emitting a child.
    assert!(
        m.step_effect().is_none(),
        "no child effect is emitted beyond the depth budget"
    );
    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().expect("outcome")["code"], "delegate_depth");
    let _ = ctx;
}
