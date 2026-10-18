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
    // Storing the answer advances the walk itself: internal signals and
    // audit bookkeeping, but no I/O.
    assert!(
        fx.iter()
            .all(|fx| matches!(fx, Effect::EmitInternal { .. } | Effect::PersistAudit)),
        "storing the answer advances without I/O: {fx:?}"
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

#[test]
fn a_json_answer_in_a_code_fence_is_accepted_as_the_answer() {
    // Local models routinely wrap the object in a markdown fence even when
    // told not to; the fence is formatting, the content is the answer.
    for text in [
        "```json\n{\"greeting\": \"hi Ada\"}\n```",
        "\n\n```\n{\"greeting\": \"hi Ada\"}\n```\n",
    ] {
        let (mut m, mut ctx, mut state) =
            start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
        let _ = m.step_effect().expect("effect");
        let _ = drive(&mut m, &mut ctx, &mut state, turn("harness.shaped", text));
        assert_ne!(state, HarnessState::Failed, "{text:?}");
        assert_eq!(
            ctx.fact(RUN_STATE_FACT).and_then(|v| v
                .get("steps")
                .and_then(|s| s.get("ask"))
                .map(|s| s["value"].clone())),
            Some(serde_json::json!({"greeting": "hi Ada"})),
            "{text:?}"
        );
    }
}

#[test]
fn text_around_a_fenced_answer_is_not_an_answer() {
    let (mut m, mut ctx, mut state) = start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("effect");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(
            "harness.shaped",
            "Sure!\n```json\n{\"greeting\": \"hi\"}\n```",
        ),
    );
    assert_eq!(state, HarnessState::Failed);
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
    // Storing the child's result advances the walk itself: internal signals
    // and audit bookkeeping, but no I/O.
    assert!(
        fx.iter()
            .all(|fx| matches!(fx, Effect::EmitInternal { .. } | Effect::PersistAudit)),
        "storing the result advances without I/O: {fx:?}"
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

/// A parked run ignores a tick: a run with I/O in flight does not advance
/// its pure chain, and re-executing a parked side effect would emit a
/// second child for the same step.
#[test]
fn a_tick_while_a_delegate_child_is_in_flight_is_ignored() {
    let (mut m, mut ctx, mut state) = start(CALLER, serde_json::json!({}));
    let first = m
        .step_effect()
        .expect("the delegate step emitted its child effect")
        .clone();
    let Effect::InstantiateSubmachine { machine, .. } = &first else {
        panic!("delegate emits InstantiateSubmachine, got {first:?}");
    };
    assert_eq!(state, HarnessState::WaitingForChild);

    let fx = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert!(fx.is_empty(), "a tick does not re-execute a parked step");
    assert_eq!(state, HarnessState::WaitingForChild);
    // The child record still names the one machine the step emitted.
    let child = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v.get("child").cloned())
        .expect("the child record survives the ignored tick");
    assert_eq!(child["machine"], machine.as_uuid().to_string());
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

/// Extracts the branch children's machine ids from the open join record in
/// the run state: (branch step id, machine uuid) pairs in declared order.
fn join_children(ctx: &sven_hsm::Context) -> Vec<(String, String)> {
    ctx.fact(RUN_STATE_FACT)
        .and_then(|v| v.get("join").map(|j| j["machines"].clone()))
        .and_then(|v| serde_json::from_value::<Vec<(String, String)>>(v).ok())
        .expect("the join record is present while the join waits")
}

/// A child completion keyed by the machine id as the join record keeps it.
fn child_completed_id(machine: &str, result: serde_json::Value) -> Event {
    Event::Internal(sven_hsm::event::InternalEvent::SubmachineCompleted {
        machine: machine.to_string(),
        result,
    })
}

#[test]
fn a_parallel_step_fans_out_one_child_per_branch_and_joins_in_declared_order() {
    let (mut m, mut ctx, mut state) = start(FANOUT, serde_json::json!({}));
    let fx = m.step_effect().expect("the fan-out emitted effects");
    let Effect::InstantiateSubmachine { .. } = fx else {
        panic!("parallel fans out children, got {fx:?}");
    };
    assert_eq!(state, HarnessState::WaitingForJoin);

    // One child per branch, in declared order, each with its own machine
    // id; the record names the parallel step it belongs to.
    let children = join_children(&ctx);
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].0, "left");
    assert_eq!(children[1].0, "right");
    assert_ne!(children[0].1, children[1].1);

    // Completing in REVERSE declared order: the first completion settles
    // nothing (the join is still missing a branch) ...
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed_id(&children[1].1, serde_json::json!({ "side": "right" })),
    );
    assert!(fx.is_empty(), "an unjoined branch advances nothing");
    assert_eq!(state, HarnessState::WaitingForJoin);
    assert_eq!(
        ctx.fact(RUN_STATE_FACT)
            .and_then(|v| v.get("steps").map(|s| s.get("split").is_none().to_string()))
            .as_deref(),
        Some("true"),
        "the join stores nothing until every branch is in"
    );

    // ... the second completion fills the join: the branch results land
    // under the parallel step's value in DECLARED order, independent of
    // completion timing, and the walk follows to the join step.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed_id(&children[0].1, serde_json::json!({ "side": "left" })),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        m.outcome().expect("outcome")["results"],
        serde_json::json!([{ "side": "left" }, { "side": "right" }]),
        "the join aggregate is the branches in declared order"
    );
}

#[test]
fn a_foreign_child_completion_does_not_fill_a_join_slot() {
    let (mut m, mut ctx, mut state) = start(FANOUT, serde_json::json!({}));
    let _ = m.step_effect().expect("effects");
    let children = join_children(&ctx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed_id(
            "00000000-0000-4000-8000-000000000000",
            serde_json::json!("late"),
        ),
    );
    // The join still waits for both real branches.
    assert_eq!(state, HarnessState::WaitingForJoin);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed_id(&children[0].1, serde_json::json!({ "side": "left" })),
    );
    assert_eq!(state, HarnessState::WaitingForJoin);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed_id(&children[1].1, serde_json::json!({ "side": "right" })),
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    let _ = ctx;
}

#[test]
fn a_parallel_step_that_fails_to_start_fails_the_run() {
    let (mut m, mut ctx, mut state) = start(FANOUT, serde_json::json!({}));
    let _ = m.step_effect().expect("effects");
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
    assert_eq!(m.outcome().expect("outcome")["code"], "side_effect_failed");
}
