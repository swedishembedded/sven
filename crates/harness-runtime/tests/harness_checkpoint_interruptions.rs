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

/// A model turn, then a tool call that writes: the write is the side effect
/// that must happen once.
const ASK_THEN_WRITE: &str = r#"
api_version: "1"
name: writer
entry: ask
capabilities: [WriteFile]
limits: { max_steps: 50, max_model_turns: 3, max_tool_calls: 3 }
steps:
  ask:
    predict: { prompt: "What to write?", store_as: text, then: write }
  write:
    tool: { name: write_file, args: { path: "out.txt", content: "{{ facts.text }}" }, store_as: written, then: out }
  out:
    return: { value: { written: "{{ steps.write.value }}" } }
"#;

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
    HarnessMachine::restore(yaml.to_string(), m.snapshot(ctx)).expect("the same document restores")
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
fn interrupted_during_a_tool_call_the_write_is_never_issued_again() {
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
                observation: json!("ok"),
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

const FANOUT: &str = r#"
api_version: "1"
name: fanout
entry: split
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3, max_parallel: 4 }
steps:
  split:
    parallel: { branches: [left, right], join: merge }
  left:
    return: { value: { side: "left" } }
  right:
    return: { value: { side: "right" } }
  merge:
    return: { value: { results: "{{ steps.split.value }}" } }
"#;

fn join_machines(ctx: &Context) -> Vec<String> {
    let machines: Vec<(String, String)> = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v.get("join").map(|j| j["machines"].clone()))
        .and_then(|v| serde_json::from_value(v).ok())
        .expect("the join record is open");
    machines.into_iter().map(|(_, id)| id).collect()
}

fn spawned(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|fx| matches!(fx, Effect::InstantiateSubmachine { .. }))
        .count()
}

#[test]
fn interrupted_mid_fan_out_the_resumed_join_spawns_no_second_batch() {
    let (m, mut ctx, mut state) = start(FANOUT, json!({}));
    let children = join_machines(&ctx);
    let (mut m, mut ctx_after) = {
        let mut m = m;
        // One branch finished before the interruption.
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::Internal(sven_hsm::event::InternalEvent::SubmachineCompleted {
                machine: children[0].clone(),
                result: json!({ "side": "left" }),
            }),
        );
        restore(FANOUT, &m, &ctx)
    };
    assert!(m.step_effect().is_none(), "no child is spawned again");

    let mut state = HarnessState::WaitingForJoin;
    let mut effects = drive(
        &mut m,
        &mut ctx_after,
        &mut state,
        Event::Internal(sven_hsm::event::InternalEvent::SubmachineCompleted {
            machine: children[1].clone(),
            result: json!({ "side": "right" }),
        }),
    );
    effects.extend(settle_ticks(&mut m, &mut ctx_after, &mut state));
    assert_eq!(spawned(&effects), 0, "{effects:?}");
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        m.outcome().expect("outcome")["results"],
        json!([{ "side": "left" }, { "side": "right" }]),
        "the branch finished before the checkpoint is kept"
    );
}

#[test]
fn a_branch_completion_redelivered_after_resume_cannot_overwrite_the_join() {
    let (mut m, mut ctx, mut state) = start(FANOUT, json!({}));
    let children = join_machines(&ctx);
    let done = |machine: &str, side: &str| {
        Event::Internal(sven_hsm::event::InternalEvent::SubmachineCompleted {
            machine: machine.to_string(),
            result: json!({ "side": side }),
        })
    };
    let _ = drive(&mut m, &mut ctx, &mut state, done(&children[0], "left"));
    let (mut m, mut ctx) = restore(FANOUT, &m, &ctx);
    let mut state = HarnessState::WaitingForJoin;

    // The executor redelivers the first branch's completion with a different
    // payload; the slot is already settled.
    let _ = drive(&mut m, &mut ctx, &mut state, done(&children[0], "forged"));
    let _ = drive(&mut m, &mut ctx, &mut state, done(&children[1], "right"));
    let _ = settle_ticks(&mut m, &mut ctx, &mut state);
    assert_eq!(
        m.outcome().expect("outcome")["results"],
        json!([{ "side": "left" }, { "side": "right" }]),
        "a settled branch is final"
    );
    let completions = m
        .trace(&ctx)
        .iter()
        .filter(|r| matches!(r, TraceRecord::ChildCompleted { .. }))
        .count();
    assert_eq!(completions, 2, "each branch completes once in the trace");
}

#[test]
fn a_delegated_child_completion_redelivered_after_resume_settles_once() {
    const DELEGATE: &str = r#"
api_version: "1"
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
api_version: "1"
name: saver
entry: save
capabilities: [WriteFile]
limits: { max_steps: 20 }
steps:
  save:
    tool: { name: write_notes, args: { path: "n.txt" }, store_as: saved, then: out }
  out:
    return: { value: "{{ steps.save.value }}" }
"#;

fn approval_of(call_id: ToolCallId) -> ApprovalId {
    ApprovalId::from_uuid(call_id.as_uuid())
}

#[test]
fn interrupted_while_waiting_for_approval_nothing_runs_until_it_is_given_and_then_once() {
    let (mut m, mut ctx, mut state) = start(GUARDED_WRITE, json!({}));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the tool step emitted its call");
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolApprovalRequired {
            call_id,
            capability: sven_hsm::ToolCapability::WriteFile,
            description: "write n.txt".into(),
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

const RETRIED_WRITE: &str = r#"
api_version: "1"
name: retrier
entry: attempt
capabilities: [WriteFile]
limits: { max_steps: 50, max_tool_calls: 5 }
steps:
  attempt:
    retry: { attempts: 2, body: write, then: out, on_exhausted: gave_up }
  write:
    tool: { name: write_file, args: { path: "r.txt" }, store_as: written, then: out }
  out:
    return: { value: "ok" }
  gave_up:
    fail: { code: exhausted, message: "no attempts left" }
"#;

#[test]
fn interrupted_during_a_retry_the_attempt_budget_survives_the_resume() {
    let (mut m, mut ctx, mut state) = start(RETRIED_WRITE, json!({}));
    let mut issued = tool_calls(&drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::user_message("tick"),
    ));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the first attempt is in flight");
    };
    issued.push(call_id);
    // The first attempt fails; the retry issues the second one.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id,
            error: "disk busy".into(),
        },
    );
    let mut second = tool_calls(&fx);
    second.extend(tool_calls(&settle_ticks(&mut m, &mut ctx, &mut state)));
    assert_eq!(
        second.len(),
        1,
        "the second attempt was issued before the interruption"
    );
    let call_id = second[0];

    let (mut m, mut ctx) = restore(RETRIED_WRITE, &m, &ctx);
    let mut state = HarnessState::WaitingForTool;
    let mut effects = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id,
            error: "disk busy".into(),
        },
    );
    effects.extend(settle_ticks(&mut m, &mut ctx, &mut state));
    assert!(
        tool_calls(&effects).is_empty(),
        "the budget of 2 attempts was spent across the interruption: {effects:?}"
    );
    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().expect("outcome")["code"], "exhausted");
}
