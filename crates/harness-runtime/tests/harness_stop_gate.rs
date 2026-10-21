// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: how an agent turn stopped is readable where a `corrections` block
//! decides what to do. A check gate's argv carries `{{ tasks.<work>.stop }}`
//! (so the command can be told why the run stopped), and the block's `while`
//! condition reads the same status, for every stop status. The supervisor is
//! asked only while the agent really concluded.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in gating what an agent did on
//! how it stopped then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessState};
use sven_hsm::{Context, Effect, Event, ProposedToolCall, ToolCallId, ToolCapability};

const DOC: &str = r#"
api_version: "2"
name: stopped
inputs: { calls: number }
execution: { limits: { model_turns: 40 } }
gates:
  check: { command: [replay, check, --stopped, "{{ tasks.work.stop }}"], timeout: 30 }
  accept: { command: [replay, accept], stdin: "{{ tasks.supervisor.output }}", timeout: 30 }
workflow:
  - name: work
    task: Do it.
    allowance: "{{ inputs.calls }}"
    max_repeated_results: 3
    corrections:
      max: 2
      check: check
      while: { eq: tasks.work.stop, value: concluded }
      author:
        name: supervisor
        system: You are the supervisor.
        task: "The run stopped as {{ tasks.work.stop }}. Write the follow-up."
      accept: accept
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
"#;

const AGENT: &str = "harness.stopped";
const SUPERVISOR: &str = "harness.stopped.supervisor";

fn call(path: &str) -> ProposedToolCall {
    ProposedToolCall {
        call_id: ToolCallId::new(),
        name: "read_file".into(),
        capability: ToolCapability::ReadFile,
        args: json!({ "path": path }),
    }
}

fn proposal(calls: Vec<ProposedToolCall>) -> Event {
    Event::LlmTurnComplete {
        thread: AGENT.into(),
        text: String::new(),
        tool_calls: calls,
    }
}

fn observed(id: ToolCallId, text: &str) -> Event {
    Event::ToolSucceeded {
        call_id: id,
        observation: Value::String(text.into()),
    }
}

/// The check gate's call in `effects`: its id and argv.
fn check_call(effects: &[Effect]) -> Option<(ToolCallId, Value)> {
    effects.iter().find_map(|e| match e {
        Effect::CallTool { call_id, args, .. } if args["argv"][0] == "replay" => {
            Some((*call_id, args["argv"].clone()))
        }
        _ => None,
    })
}

fn read_calls(effects: &[Effect]) -> Vec<ToolCallId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::CallTool { call_id, args, .. } if args["argv"].is_null() => Some(*call_id),
            _ => None,
        })
        .collect()
}

/// Runs the agent's turn to the given stop status and returns the machine
/// where the check gate has just been called, with that call.
fn stopped_as(status: &str) -> (HarnessMachine, Context, HarnessState, (ToolCallId, Value)) {
    let calls = if status == "tool_cap" { 2 } else { 50 };
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "calls": calls }));
    let mut effects = match status {
        "concluded" => drive(&mut m, &mut ctx, &mut state, turn(AGENT, "All done.")),
        "stopped_without_conclusion" => {
            drive(&mut m, &mut ctx, &mut state, turn(AGENT, "Now the tests:"))
        }
        "output_truncated" => {
            let _ = drive(
                &mut m,
                &mut ctx,
                &mut state,
                Event::Internal(sven_hsm::InternalEvent::Custom {
                    name: sven_hsm::TURN_TRUNCATED.to_string(),
                    payload: Value::Null,
                }),
            );
            drive(&mut m, &mut ctx, &mut state, turn(AGENT, ""))
        }
        "tool_cap" => {
            let fx = drive(
                &mut m,
                &mut ctx,
                &mut state,
                proposal(vec![call("a"), call("b"), call("c")]),
            );
            let mut last = Vec::new();
            for id in read_calls(&fx) {
                last = drive(&mut m, &mut ctx, &mut state, observed(id, "x"));
            }
            last
        }
        "repetition" => {
            let mut last = Vec::new();
            for _ in 0..3 {
                let c = call("same");
                let id = c.call_id;
                let _ = drive(&mut m, &mut ctx, &mut state, proposal(vec![c]));
                last = drive(&mut m, &mut ctx, &mut state, observed(id, "unchanged"));
            }
            last
        }
        other => panic!("no such stop {other}"),
    };
    effects.retain(|e| matches!(e, Effect::CallTool { .. }));
    let gate = check_call(&effects)
        .unwrap_or_else(|| panic!("the check gate runs after a turn that stopped as {status}"));
    (m, ctx, state, gate)
}

fn failed_check() -> Value {
    json!({ "exit_code": 1, "timed_out": false, "error": null, "stdout": "", "stderr": "" })
        .to_string()
        .into()
}

#[test]
fn the_check_gate_is_told_why_the_agent_stopped_and_while_follows_it() {
    for status in [
        "concluded",
        "stopped_without_conclusion",
        "output_truncated",
        "tool_cap",
        "repetition",
    ] {
        let (mut m, mut ctx, mut state, (check, argv)) = stopped_as(status);
        assert_eq!(
            argv,
            json!(["replay", "check", "--stopped", status]),
            "the gate's argv names the stop"
        );
        let fx = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id: check,
                observation: failed_check(),
            },
        );
        let asks_supervisor = fx
            .iter()
            .any(|e| matches!(e, Effect::CallLlm { request } if request["thread"] == SUPERVISOR));
        if status == "concluded" {
            assert!(asks_supervisor, "a concluded run is corrected: {fx:?}");
            let ask = fx
                .iter()
                .find_map(|e| match e {
                    Effect::CallLlm { request } => request["instruction"].as_str(),
                    _ => None,
                })
                .unwrap();
            assert!(ask.contains("stopped as concluded"), "{ask}");
        } else {
            assert!(!asks_supervisor, "{status}: the loop ends, {fx:?}");
            assert_eq!(state, HarnessState::Completed, "{status}");
            assert_eq!(m.outcome().unwrap()["stop"], status);
        }
    }
}
