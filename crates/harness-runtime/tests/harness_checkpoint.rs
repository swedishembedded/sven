// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness checkpoint's spec: a snapshot resumes at the step it stopped
//! on, and only under the plan it was taken against — a checkpoint records
//! the digest of the plan it started under, and a modified document cannot
//! silently replace the checkpointed version.

mod common;

use common::{drive, start, turn};
use sven_harness_runtime::{HarnessMachine, RUN_STATE_FACT};
use sven_hsm::{Effect, Event, Machine};

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
      then: save
  save:
    assign:
      set:
        spoken: "{{ steps.ask.value }}"
      then: out
  out:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

/// The same harness, edited after the fact: the plan a checkpoint recorded
/// is no longer the plan this document compiles to.
const GREET_EDITED: &str = r#"
api_version: "2"
name: greet
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Say goodbye to {{ inputs.name }}"
      store_as: greeting
      then: save
  save:
    assign:
      set:
        spoken: "never {{ steps.ask.value }}"
      then: out
  out:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

#[test]
fn a_started_run_records_the_digest_of_the_plan_it_started_under() {
    let (m, ctx, _state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let digest = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v.get("plan_digest").and_then(|d| d.as_str()))
        .and_then(|d| {
            if d.is_empty() {
                None
            } else {
                Some(d.to_string())
            }
        })
        .expect("the run state names the plan it started under");
    let plan = common::plan(GREET).expect("compiles");
    assert_eq!(
        digest, plan.digest,
        "the recorded digest is the compiled plan's digest"
    );
    let _ = &m;
}

#[test]
fn a_checkpoint_resumes_at_the_step_it_stopped_on() {
    // The checkpoint is taken with the turn in flight: the walk parked on
    // the predict step and nothing has come back yet.
    let (m, ctx, _state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    assert!(
        matches!(m.step_effect(), Some(Effect::CallLlm { .. })),
        "the walk parks on the predict step"
    );
    let snap = m.snapshot(&ctx);
    let (mut m2, mut ctx2) =
        common::restore(GREET, snap).expect("the unchanged document restores its own checkpoint");
    assert_eq!(
        ctx2.fact(RUN_STATE_FACT)
            .and_then(|v| v.get("current").and_then(|c| c.as_str())),
        Some("ask"),
        "the checkpoint resumes at the stored step, not the entry"
    );

    // The recorded turn settles the restored run, which walks on by itself.
    let mut state = sven_harness_runtime::HarnessState::WaitingForModel;
    let _ = drive(
        &mut m2,
        &mut ctx2,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(
        m2.outcome().expect("finished")["message"],
        serde_json::json!("Hello Ada"),
        "the resumed run's answer is the one the checkpoint carried"
    );
}

#[test]
fn a_modified_document_cannot_silently_replace_a_checkpointed_plan() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("predict emitted its turn");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );

    let snap = m.snapshot(&ctx);
    let text = match common::restore(GREET_EDITED, snap) {
        Err(err) => err.to_string(),
        Ok(_) => panic!("the edited document is not the checkpointed plan"),
    };
    assert!(
        text.contains("greet"),
        "the refusal names the harness: {text}"
    );
    assert!(
        text.contains("digest"),
        "the refusal says why: the plan's digest no longer matches: {text}"
    );
}

/// A harness that delegates: its replayable event log contains the child's
/// completion, which names the machine id the parent's own emission chose.
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

/// Drives a fresh machine through `events`, returning the effects the run
/// emitted (as an executor would collect them, executing nothing).
fn run_events(
    events: &[Event],
) -> (
    HarnessMachine,
    sven_hsm::Context,
    sven_harness_runtime::HarnessState,
    Vec<Effect>,
) {
    let run = common::data(DELEGATOR, serde_json::json!({})).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = sven_hsm::Context::new();
    let mut state = m.initial();
    let mut effects = Vec::new();
    for event in events {
        effects.extend(drive(&mut m, &mut ctx, &mut state, event.clone()));
    }
    (m, ctx, state, effects)
}

#[test]
fn a_delegate_run_replays_from_its_recorded_events() {
    // Record: the run's own emission names the child machine id; the
    // recorded completion carries that id, as a real executor's would.
    let (_, _, state, fx) = run_events(&[Event::user_message("start")]);
    assert_eq!(state, sven_harness_runtime::HarnessState::WaitingForChild);
    let child = fx
        .iter()
        .find_map(|fx| match fx {
            Effect::InstantiateSubmachine { machine, .. } => Some(*machine),
            _ => None,
        })
        .expect("the delegate step spawned a child");
    let events = vec![
        Event::user_message("start"),
        common::child_completed(&child, serde_json::json!({ "found": "spade" })),
        Event::user_message("tick"),
    ];

    // Two independent reconstructions from the same recorded log.
    let (m1, ctx1, state1, fx1) = run_events(&events);
    let (m2, ctx2, state2, fx2) = run_events(&events);
    assert_eq!(
        state1,
        sven_harness_runtime::HarnessState::Completed,
        "replay 1 completes"
    );
    assert_eq!(
        state2,
        sven_harness_runtime::HarnessState::Completed,
        "replay 2 completes"
    );
    assert_eq!(
        ctx1.fact(RUN_STATE_FACT),
        ctx2.fact(RUN_STATE_FACT),
        "both reconstructions reach the same logical state"
    );
    assert_eq!(fx1, fx2, "both reconstructions emit the same effects");
    assert_eq!(m1.outcome(), m2.outcome());
    assert_eq!(
        m1.outcome().map(|o| o["found"].clone()),
        Some(serde_json::json!({ "found": "spade" })),
        "a delegate stores the child's whole result as the step's value"
    );
}

#[test]
fn two_children_of_one_run_are_named_differently() {
    const TWICE: &str = r#"
api_version: "2"
name: twice
entry: first
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 2 }
steps:
  first:
    delegate:
      target: helper
      input: { task: "one" }
      then: second
  second:
    delegate:
      target: helper
      input: { task: "two" }
      then: done
  done:
    return:
      value: "ok"
"#;
    let run = common::data(TWICE, serde_json::json!({})).expect("valid");
    let mut m = HarnessMachine::new(run);
    let mut ctx = sven_hsm::Context::new();
    let mut state = m.initial();
    let mut ids = Vec::new();
    let fx = drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    let first = find_child(&fx).expect("the first delegate spawned a child");
    ids.push(first.as_uuid().to_string());
    // The first child's completion advances the walk, which spawns the
    // second child in the same drive (the advance signal bounce).
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        common::child_completed(&first, serde_json::json!({})),
    );
    let second = find_child(&fx).expect("the second delegate spawned a child");
    ids.push(second.as_uuid().to_string());
    assert_ne!(
        ids[0], ids[1],
        "two children of one run are distinct machines"
    );
    let _ = m;
}

fn find_child(fx: &[Effect]) -> Option<sven_hsm::MachineId> {
    fx.iter().find_map(|fx| match fx {
        Effect::InstantiateSubmachine { machine, .. } => Some(*machine),
        _ => None,
    })
}

/// A run that asks the model and then runs a gate: the replay topology that
/// exercises both waiting states.
const ASKER: &str = r#"
api_version: "2"
name: asker
entry: ask
capabilities: [ExecuteShell]
limits: { max_steps: 50, max_model_turns: 5, max_tool_calls: 5 }
steps:
  ask:
    predict:
      prompt: "What shall I read?"
      store_as: subject
      then: read
  read:
    gate:
      steps: [{ name: cat, argv: [cat, "{{ facts.subject }}"] }]
      then: out
      on_fail: bad
  out:
    return:
      value:
        subject: "{{ facts.subject }}"
        text: "{{ steps.read.stdout }}"
  bad:
    fail: { code: gate_failed }
"#;

#[test]
fn a_model_and_gate_run_replays_from_its_recorded_events() {
    // The recorded log: every completion event the original run's
    // executors posted. The completions are inputs to the replay; the
    // effects the replay emits (its turns and calls) are collected, never
    // executed - nothing here runs a tool.
    let events = vec![
        Event::user_message("start"),
        turn("harness.asker", "read the notes"),
        Event::ToolSucceeded {
            call_id: sven_harness_runtime::step_call_id("asker", "read"),
            observation: report("the notes"),
        },
        Event::user_message("tick"),
    ];
    let (m1, ctx1, state1, fx1) = run_asker(&events);
    let (m2, ctx2, state2, fx2) = run_asker(&events);
    assert_eq!(state1, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(state2, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(
        ctx1.fact(RUN_STATE_FACT),
        ctx2.fact(RUN_STATE_FACT),
        "both reconstructions reach the same logical state"
    );
    assert_eq!(
        fx1, fx2,
        "both reconstructions emit the same turns and calls"
    );
    assert_eq!(m1.outcome(), m2.outcome());
    assert_eq!(
        m1.outcome().map(|o| o["subject"].clone()),
        Some(serde_json::json!("read the notes"))
    );
}

/// The gate tool's report of a command that exited 0 with `stdout`.
fn report(stdout: &str) -> serde_json::Value {
    serde_json::json!({ "exit_code": 0, "timed_out": false, "error": null,
                        "stdout": stdout, "stderr": "" })
    .to_string()
    .into()
}

/// Drives a fresh `ASKER` machine through `events`, collecting the effects
/// it emits (as an executor would observe them, executing nothing).
fn run_asker(
    events: &[Event],
) -> (
    HarnessMachine,
    sven_hsm::Context,
    sven_harness_runtime::HarnessState,
    Vec<Effect>,
) {
    let run = common::data(ASKER, serde_json::json!({})).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = sven_hsm::Context::new();
    let mut state = m.initial();
    let mut effects = Vec::new();
    for event in events {
        effects.extend(drive(&mut m, &mut ctx, &mut state, event.clone()));
    }
    (m, ctx, state, effects)
}

#[test]
fn a_restored_run_never_repeats_the_operation_in_flight() {
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
    let (m, ctx, _state) = start(READER, serde_json::json!({}));
    let expected = sven_harness_runtime::step_call_id("reader", "fetch");
    let snap = m.snapshot(&ctx);

    // The restore itself emits nothing: the call already left the process
    // before the checkpoint, and re-emitting it would run the tool twice.
    let (mut m2, mut ctx2) =
        common::restore(READER, snap).expect("the unchanged document restores its own checkpoint");
    assert!(
        m2.step_effect().is_none(),
        "a restored machine does not re-emit the call in flight"
    );
    let mut state = sven_harness_runtime::HarnessState::WaitingForTool;
    let mut effects = Vec::new();
    effects.extend(drive(
        &mut m2,
        &mut ctx2,
        &mut state,
        Event::ToolSucceeded {
            call_id: expected,
            observation: report("the notes"),
        },
    ));
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        effects.extend(drive(
            &mut m2,
            &mut ctx2,
            &mut state,
            Event::user_message("tick"),
        ));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert!(
        effects
            .iter()
            .all(|fx| !matches!(fx, Effect::CallTool { .. })),
        "the resumed run never calls the tool again: {effects:?}"
    );
    let trace = m2.trace(&ctx2);
    assert_eq!(
        trace
            .iter()
            .filter(|r| matches!(
                r,
                sven_harness_runtime::TraceRecord::StepCompleted { step, .. } if step.as_str() == "fetch"
            ))
            .count(),
        1,
        "the in-flight operation settled exactly once: {trace:?}"
    );
}
