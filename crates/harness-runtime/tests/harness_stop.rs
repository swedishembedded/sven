// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent turn ends with a status a later step can read (concluded,
//! stopped without conclusion, tool cap, repetition); a follow-up is sent
//! only while its condition holds and up to `max` times; a task's tool-call
//! allowance is set from an input, spent across the task's turns, and a call
//! beyond it ends the turn instead of failing the run.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessState};
use sven_hsm::{Context, Effect, Event, ProposedToolCall, ToolCallId, ToolCapability};

const DOC: &str = r#"
api_version: "2"
name: stops
inputs: { calls: number }
execution: { limits: { model_turns: 40 } }
workflow:
  - name: work
    allowance: "{{ inputs.calls }}"
    max_repeated_results: 3
    task: Do it.
    follow_ups:
      - task: You stopped. Make the call or say you are done.
        when: { last_turn: stopped_without_conclusion }
        max: 2
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
  left: { type: number, value: "{{ tasks.work.calls_left }}" }
  said: { type: string, value: "{{ tasks.work.output }}" }
"#;

const THREAD: &str = "harness.stops";

fn call(name: &str, path: &str) -> ProposedToolCall {
    ProposedToolCall {
        call_id: ToolCallId::new(),
        name: name.into(),
        capability: ToolCapability::ReadFile,
        args: json!({ "path": path }),
    }
}

fn proposal(calls: Vec<ProposedToolCall>) -> Event {
    Event::LlmTurnComplete {
        thread: THREAD.into(),
        text: String::new(),
        tool_calls: calls,
    }
}

fn instruction(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => request["instruction"].as_str().map(str::to_string),
        _ => None,
    })
}

fn tool_calls(effects: &[Effect]) -> Vec<ToolCallId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::CallTool { call_id, .. } => Some(*call_id),
            _ => None,
        })
        .collect()
}

fn begin(calls: u64) -> (HarnessMachine, Context, HarnessState) {
    start(DOC, json!({ "calls": calls }))
}

#[test]
fn a_turn_that_announces_work_it_did_not_do_earns_the_follow_up_up_to_max_times() {
    let (mut m, mut ctx, mut state) = begin(10);
    let fx = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "Now the tests:"));
    assert_eq!(
        instruction(&fx).as_deref(),
        Some("You stopped. Make the call or say you are done.")
    );
    // Still announcing: the same follow-up is sent a second time.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(THREAD, "Next, the build:"),
    );
    assert!(
        instruction(&fx).is_some_and(|i| i.starts_with("You stopped")),
        "second follow-up"
    );
    // And a third time it is not: max is two.
    let _ = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "And then:"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(m.outcome().unwrap()["stop"], "stopped_without_conclusion");
}

#[test]
fn a_turn_that_concludes_sends_no_follow_up() {
    let (mut m, mut ctx, mut state) = begin(10);
    let _ = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "All done."));
    assert_eq!(state, HarnessState::Completed);
    let outcome = m.outcome().unwrap();
    assert_eq!(outcome["stop"], "concluded");
    assert_eq!(outcome["said"], "All done.");
}

#[test]
fn a_call_beyond_the_allowance_is_refused_and_ends_the_turn_with_a_status() {
    let (mut m, mut ctx, mut state) = begin(2);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        proposal(vec![
            call("read_file", "a"),
            call("read_file", "b"),
            call("read_file", "c"),
        ]),
    );
    let ran = tool_calls(&fx);
    assert_eq!(ran.len(), 2, "only the allowance runs: {fx:?}");
    for id in ran {
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: id,
                observation: "x".into(),
            },
        );
    }
    assert_eq!(
        state,
        HarnessState::Completed,
        "the turn ended; the run did not fail"
    );
    let outcome = m.outcome().unwrap();
    assert_eq!(outcome["stop"], "tool_cap");
    assert_eq!(outcome["left"], 0);
}

#[test]
fn the_allowance_is_spent_across_the_turns_of_a_task() {
    let (mut m, mut ctx, mut state) = begin(3);
    // The first turn uses two calls, then announces more work.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        proposal(vec![call("read_file", "a"), call("read_file", "b")]),
    );
    for id in tool_calls(&fx) {
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: id,
                observation: format!("{id:?}").into(),
            },
        );
    }
    let fx = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "Now the rest:"));
    assert!(instruction(&fx).is_some_and(|i| i.starts_with("You stopped")));
    // One call is left for the follow-up; a second is beyond it.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        proposal(vec![call("read_file", "c"), call("read_file", "d")]),
    );
    assert_eq!(tool_calls(&fx).len(), 1);
}

#[test]
fn the_same_call_answered_the_same_way_again_and_again_ends_the_turn() {
    let (mut m, mut ctx, mut state) = begin(50);
    for round in 0..3 {
        let c = call("read_file", "same");
        let id = c.call_id;
        let fx = drive(&mut m, &mut ctx, &mut state, proposal(vec![c]));
        assert_eq!(tool_calls(&fx), vec![id], "round {round} runs");
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: id,
                observation: Value::String("unchanged".into()),
            },
        );
    }
    assert_eq!(
        state,
        HarnessState::Completed,
        "the third identical answer ends the turn"
    );
    assert_eq!(m.outcome().unwrap()["stop"], "repetition");
}

#[test]
fn a_different_answer_does_not_count_as_a_repetition() {
    let (mut m, mut ctx, mut state) = begin(50);
    for round in 0..4 {
        let c = call("read_file", "same");
        let id = c.call_id;
        let _ = drive(&mut m, &mut ctx, &mut state, proposal(vec![c]));
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: id,
                observation: format!("version {round}").into(),
            },
        );
    }
    assert_ne!(state, HarnessState::Completed);
}

#[test]
fn an_allowance_that_is_not_a_whole_number_fails_the_run_and_says_so() {
    let (m, _, state) = start(DOC, json!({ "calls": "many" }));
    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().unwrap()["code"], "invalid_setting");
}

#[test]
fn the_tool_call_limit_can_come_from_the_runs_inputs() {
    let doc = r#"
api_version: "2"
name: limited
inputs: { calls: number }
execution: { limits: { model_turns: 10, tool_calls: "{{ inputs.calls }}" } }
gates:
  g: { command: [t] }
workflow:
  - name: work
    task: Do it.
    verify: [g]
    repair: { attempts: 3 }
"#;
    // Two gate calls are allowed; the third (the second repair's check) is not.
    let (mut m, mut ctx, mut state) = start(doc, json!({ "calls": 2 }));
    let mut fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.limited", "done"),
    );
    let mut ran = 0;
    while !state.is_terminal() && ran < 10 {
        let Some(id) = tool_calls(&fx).first().copied() else {
            break;
        };
        ran += 1;
        let failed: Value = json!({ "exit_code": 1, "timed_out": false, "error": null })
            .to_string()
            .into();
        fx = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: id,
                observation: failed,
            },
        );
        if fx.iter().any(|e| matches!(e, Effect::CallLlm { .. })) {
            fx = drive(
                &mut m,
                &mut ctx,
                &mut state,
                turn("harness.limited", "again"),
            );
        }
    }
    assert_eq!(ran, 2, "no third gate call");
    assert_eq!(m.outcome().unwrap()["code"], "tool_call_budget");
}
