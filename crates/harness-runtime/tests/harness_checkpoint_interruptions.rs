// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Checkpoint qualification: a run interrupted while a model turn, a tool
//! call, a parallel delegation, an approval or a retry is in flight resumes
//! without repeating an irreversible side effect, and an at-least-once
//! delivery of a completion the run already consumed settles nothing twice.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

mod common;

use common::{child_completed, drive, start, turn};
use serde_json::json;
use sven_harness_runtime::{HarnessMachine, HarnessState, TraceRecord, RUN_STATE_FACT};
use sven_hsm::ids::{ApprovalId, ToolCallId};
use sven_hsm::{Context, Effect, Event};

/// A model turn, then a gate that writes: the write is the side effect that
/// must happen once.
const ASK_THEN_WRITE: &str = r#"
api_version: "2"
name: writer
entry: ask
capabilities: [ExecuteShell]
limits: { max_steps: 50, max_model_turns: 3, max_tool_calls: 3 }
steps:
  ask:
    predict: { prompt: "What to write?", store_as: text, then: write }
  write:
    gate:
      steps: [{ name: tee, argv: [tee, out.txt] }]
      stdin: "{{ facts.text }}"
      then: out
      on_fail: bad
  out:
    return: { value: { written: "{{ steps.write.stdout }}" } }
  bad:
    fail: { code: gate_failed }
"#;

/// The gate tool's report of a command that exited 0.
fn report() -> serde_json::Value {
    json!({ "exit_code": 0, "timed_out": false, "error": null, "stdout": "ok", "stderr": "" })
        .to_string()
        .into()
}

fn tool_calls(effects: &[Effect]) -> Vec<ToolCallId> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            Effect::CallTool { call_id, .. } => Some(*call_id),
            _ => None,
        })
        .collect()
}

fn restore(yaml: &str, m: &HarnessMachine, ctx: &Context) -> (HarnessMachine, Context) {
    common::restore(yaml, m.snapshot(ctx)).expect("the same document restores")
}

fn settle_ticks(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    state: &mut HarnessState,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    for _ in 0..8 {
        if matches!(state, HarnessState::Completed | HarnessState::Failed) {
            break;
        }
        effects.extend(drive(m, ctx, state, Event::user_message("tick")));
    }
    effects
}

fn completed_steps(m: &HarnessMachine, ctx: &Context, step: &str) -> usize {
    m.trace(ctx)
        .iter()
        .filter(|r| matches!(r, TraceRecord::StepCompleted { step: s, .. } if s.as_str() == step))
        .count()
}

#[test]
fn interrupted_during_generation_the_resumed_run_asks_the_model_nothing_again() {
    let (m, ctx, _) = start(ASK_THEN_WRITE, json!({}));
    let (mut m, mut ctx) = restore(ASK_THEN_WRITE, &m, &ctx);
    assert!(
        m.step_effect().is_none(),
        "the turn in flight is not re-asked"
    );

    let mut state = HarnessState::WaitingForModel;
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.writer", "hello"),
    );
    let mut writes = tool_calls(&fx);
    writes.extend(tool_calls(&settle_ticks(&mut m, &mut ctx, &mut state)));
    assert_eq!(
        writes.len(),
        1,
        "the settled turn leads to exactly one write"
    );
    assert_eq!(state, HarnessState::WaitingForTool);
}

#[test]
fn a_model_turn_delivered_twice_does_not_write_twice() {
    let (mut m, mut ctx, mut state) = start(ASK_THEN_WRITE, json!({}));
    let mut writes = Vec::new();
    for _ in 0..2 {
        let fx = drive(
            &mut m,
            &mut ctx,
            &mut state,
            turn("harness.writer", "hello"),
        );
        writes.extend(tool_calls(&fx));
        writes.extend(tool_calls(&settle_ticks(&mut m, &mut ctx, &mut state)));
    }
    assert_eq!(
        writes.len(),
        1,
        "a duplicate delivery of the turn is ignored"
    );
    assert_eq!(completed_steps(&m, &ctx, "ask"), 1);
}

#[test]
fn interrupted_during_a_gate_the_write_is_never_issued_again() {
    let (mut m, mut ctx, mut state) = start(ASK_THEN_WRITE, json!({}));
    let mut issued = tool_calls(&drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.writer", "hello"),
    ));
    issued.extend(tool_calls(&settle_ticks(&mut m, &mut ctx, &mut state)));
    assert_eq!(issued.len(), 1, "the original run issued the write");
    let call_id = issued[0];

    // Process dies with the write in flight; the new process restores.
    let (mut m2, mut ctx2) = restore(ASK_THEN_WRITE, &m, &ctx);
    assert!(m2.step_effect().is_none());
    let mut state2 = HarnessState::WaitingForTool;
    // The tool's completion arrives twice (the executor redelivers).
    let mut effects = Vec::new();
    for _ in 0..2 {
        effects.extend(drive(
            &mut m2,
            &mut ctx2,
            &mut state2,
            Event::ToolSucceeded {
                call_id,
                observation: report(),
            },
        ));
    }
    effects.extend(settle_ticks(&mut m2, &mut ctx2, &mut state2));
    assert!(
        tool_calls(&effects).is_empty(),
        "no second write: {effects:?}"
    );
    assert_eq!(state2, HarnessState::Completed);
    assert_eq!(completed_steps(&m2, &ctx2, "write"), 1);
}

#[test]
fn a_delegated_child_completion_redelivered_after_resume_settles_once() {
    const DELEGATE: &str = r#"
api_version: "2"
name: parent
entry: ask_child
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 2 }
steps:
  ask_child:
    delegate: { target: helper, input: {}, then: out }
  out:
    return: { value: "{{ steps.ask_child.value }}" }
"#;
    let (m, ctx, _) = start(DELEGATE, json!({}));
    let child = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v["child"]["machine"].as_str().map(str::to_string))
        .expect("the child is recorded");
    let (mut m, mut ctx) = restore(DELEGATE, &m, &ctx);
    let mut state = HarnessState::WaitingForChild;
    let machine = child.parse::<uuid::Uuid>().expect("a uuid");
    let _ = machine;
    let id = sven_hsm::MachineId::from_uuid(child.parse().expect("a uuid"));
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&id, json!("first")),
    );
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        child_completed(&id, json!("second")),
    );
    let _ = settle_ticks(&mut m, &mut ctx, &mut state);
    assert_eq!(m.outcome(), Some(json!("first")));
    assert_eq!(completed_steps(&m, &ctx, "ask_child"), 1);
}

const GUARDED_WRITE: &str = r#"
api_version: "2"
name: saver
entry: save
capabilities: [ExecuteShell]
limits: { max_steps: 20 }
steps:
  save:
    gate: { steps: [{ name: write, argv: [sh, save.sh] }], then: out, on_fail: bad }
  out:
    return: { value: "saved" }
  bad:
    fail: { code: gate_failed }
"#;

fn approval_of(call_id: ToolCallId) -> ApprovalId {
    ApprovalId::from_uuid(call_id.as_uuid())
}

#[test]
fn interrupted_while_waiting_for_approval_nothing_runs_until_it_is_given_and_then_once() {
    let (mut m, mut ctx, mut state) = start(GUARDED_WRITE, json!({}));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the gate step emitted its call");
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolApprovalRequired {
            call_id,
            capability: sven_hsm::ToolCapability::ExecuteShell,
            description: "run save.sh".into(),
        },
    );
    let (mut m, mut ctx) = restore(GUARDED_WRITE, &m, &ctx);
    assert!(m.step_effect().is_none(), "restoring runs nothing");
    assert!(!ctx.is_call_approved(call_id), "approval is not inherited");
    let mut state = HarnessState::WaitingForTool;

    let approve = Event::HumanApproved {
        approval_id: approval_of(call_id),
    };
    let first = drive(&mut m, &mut ctx, &mut state, approve.clone());
    let again = drive(&mut m, &mut ctx, &mut state, approve);
    assert_eq!(tool_calls(&first), [call_id], "the approved call runs once");
    assert!(
        tool_calls(&again).is_empty(),
        "a repeated approval runs nothing: {again:?}"
    );
}
