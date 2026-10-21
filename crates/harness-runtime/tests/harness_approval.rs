// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The approval flow of a harness run: when the kernel's permission gate
//! holds a tool call for a person (manual approval), the machine asks, runs
//! the very call that was asked about once it is approved, and settles it as
//! a refusal when it is not - for a `tool` step and for an `agent` loop.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

mod common;

use common::{drive, start};
use serde_json::json;
use sven_harness_runtime::HarnessState;
use sven_hsm::ids::{ApprovalId, ToolCallId};
use sven_hsm::{Effect, Event, ProposedToolCall, ToolCapability};

const GATE_STEP: &str = r#"
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

const AGENT_STEP: &str = r#"
api_version: "2"
name: worker
entry: do
capabilities: [WriteFile]
limits: { max_steps: 20, max_model_turns: 6, max_tool_calls: 6 }
steps:
  do:
    agent: { prompt: "Write.", tools: [write_notes, write_more], then: out }
  out:
    return: { value: "{{ steps.do.value }}" }
"#;

fn approval_required(call_id: ToolCallId) -> Event {
    approval_required_for(call_id, ToolCapability::WriteFile)
}

fn approval_required_for(call_id: ToolCallId, capability: ToolCapability) -> Event {
    Event::ToolApprovalRequired {
        call_id,
        capability,
        description: "needs approval".into(),
    }
}

fn approval_of(call_id: ToolCallId) -> ApprovalId {
    ApprovalId::from_uuid(call_id.as_uuid())
}

fn requests(effects: &[Effect]) -> Vec<(ApprovalId, Option<String>)> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            Effect::RequestHumanApproval {
                approval_id, call, ..
            } => Some((*approval_id, call.as_ref().map(|c| c.name.clone()))),
            _ => None,
        })
        .collect()
}

fn calls(effects: &[Effect]) -> Vec<(ToolCallId, String)> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            Effect::CallTool { call_id, name, .. } => Some((*call_id, name.clone())),
            _ => None,
        })
        .collect()
}

fn proposal(name: &str) -> ProposedToolCall {
    ProposedToolCall {
        call_id: ToolCallId::new(),
        name: name.into(),
        args: json!({"path": name}),
        capability: ToolCapability::WriteFile,
    }
}

fn proposed(calls: Vec<ProposedToolCall>) -> Event {
    Event::LlmTurnComplete {
        thread: "harness.worker".into(),
        text: String::new(),
        tool_calls: calls,
    }
}

#[test]
fn a_held_gate_call_is_asked_then_run_once_approved() {
    let (mut m, mut ctx, mut state) = start(GATE_STEP, json!({}));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the gate step emitted its call");
    };

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        approval_required_for(call_id, ToolCapability::ExecuteShell),
    );
    assert_eq!(
        requests(&fx),
        [(approval_of(call_id), Some("shell_gate".to_string()))],
        "the person is shown the call that is held"
    );
    assert_eq!(state, HarnessState::WaitingForTool);

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::HumanApproved {
            approval_id: approval_of(call_id),
        },
    );
    assert_eq!(
        calls(&fx),
        [(call_id, "shell_gate".to_string())],
        "the approved call is issued again, as it was"
    );
    assert!(ctx.is_call_approved(call_id));

    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id,
            observation: json!({ "exit_code": 0, "timed_out": false, "error": null,
                                 "stdout": "", "stderr": "" })
            .to_string()
            .into(),
        },
    );
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
}

#[test]
fn a_refused_gate_call_fails_the_run_without_running() {
    let (mut m, mut ctx, mut state) = start(GATE_STEP, json!({}));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the gate step emitted its call");
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        approval_required_for(call_id, ToolCapability::ExecuteShell),
    );

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::HumanRejected {
            approval_id: approval_of(call_id),
            reason: None,
        },
    );

    assert!(calls(&fx).is_empty(), "a refused call never runs: {fx:?}");
    assert_eq!(state, HarnessState::Failed);
    assert!(!ctx.is_call_approved(call_id));
}

#[test]
fn an_approval_for_another_call_settles_nothing() {
    let (mut m, mut ctx, mut state) = start(GATE_STEP, json!({}));
    let Some(Effect::CallTool { call_id, .. }) = m.step_effect() else {
        panic!("the gate step emitted its call");
    };
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        approval_required_for(call_id, ToolCapability::ExecuteShell),
    );

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::HumanApproved {
            approval_id: approval_of(ToolCallId::new()),
        },
    );

    assert!(fx.is_empty(), "a stale approval is ignored: {fx:?}");
    assert!(!ctx.is_call_approved(call_id));
}

#[test]
fn gated_agent_calls_are_asked_one_at_a_time_and_the_loop_continues_after_them() {
    let (mut m, mut ctx, mut state) = start(AGENT_STEP, json!({}));
    let (first, second) = (proposal("write_notes"), proposal("write_more"));
    let (first_id, second_id) = (first.call_id, second.call_id);
    let _ = drive(&mut m, &mut ctx, &mut state, proposed(vec![first, second]));

    // The kernel holds both calls; each reports its gate.
    let fx = drive(&mut m, &mut ctx, &mut state, approval_required(first_id));
    assert_eq!(
        requests(&fx),
        [(approval_of(first_id), Some("write_notes".to_string()))]
    );
    let fx = drive(&mut m, &mut ctx, &mut state, approval_required(second_id));
    assert!(
        requests(&fx).is_empty(),
        "the second waits behind the first: {fx:?}"
    );

    // Approving the first runs it and moves on to asking about the second.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::HumanApproved {
            approval_id: approval_of(first_id),
        },
    );
    assert_eq!(calls(&fx), [(first_id, "write_notes".to_string())]);
    assert_eq!(
        requests(&fx),
        [(approval_of(second_id), Some("write_more".to_string()))]
    );

    // Refusing the second settles it as a refusal the model reads.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::HumanRejected {
            approval_id: approval_of(second_id),
            reason: None,
        },
    );
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: first_id,
            observation: json!("saved"),
        },
    );
    let turn = fx
        .iter()
        .find_map(|fx| match fx {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .expect("the loop continues once both settled");
    assert_eq!(
        turn["answered_calls"][0]["call_id"],
        second_id.as_uuid().to_string()
    );
    assert!(turn["answered_calls"][0]["content"]
        .as_str()
        .is_some_and(|r| r.contains("not approve")));
}
