// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a gate is given a task's output on standard input, keeps its latest
//! result (exit status, output, the output parsed as JSON) where later steps
//! and the outputs read it, a gate that is not required never changes the
//! course of the run, and with `on_failure: return` a failed check returns the
//! attempt's outputs marked failed.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::HarnessState;
use sven_hsm::{Effect, Event};

const DOC: &str = r#"
api_version: "2"
name: data
execution: { limits: { model_turns: 10 } }
gates:
  judge:
    command: [checker, judge]
    stdin: "{{ tasks.work.output }}"
workflow:
  - name: work
    task: Do it.
    verify: [{ gate: judge, required: false }]
outputs:
  verdict: { type: json, value: "{{ gates.judge.json }}" }
  exit: { type: number, value: "{{ gates.judge.exit_code }}" }
"#;

fn gate_call(effects: &[Effect]) -> (sven_hsm::ToolCallId, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallTool { call_id, args, .. } => Some((*call_id, args.clone())),
            _ => None,
        })
        .expect("a gate call")
}

fn report(exit: i64, stdout: &str) -> Value {
    json!({ "exit_code": exit, "timed_out": false, "error": null, "stdout": stdout, "stderr": "" })
        .to_string()
        .into()
}

#[test]
fn a_gate_that_is_not_required_hands_its_findings_on_as_data_and_the_run_goes_on() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({}));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.data", "my changes"),
    );
    let (call, args) = gate_call(&fx);
    assert_eq!(
        args["stdin"], "my changes",
        "the task's output is the gate's input"
    );

    let findings = r#"{"classes":["wrong_file"],"correctable":true}"#;
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call,
            observation: report(1, findings),
        },
    );
    assert_eq!(
        state,
        HarnessState::Completed,
        "a failed check that is not required does not fail the run"
    );
    let outcome = m.outcome().expect("outcome");
    assert_eq!(
        outcome["verdict"],
        json!({ "classes": ["wrong_file"], "correctable": true })
    );
    assert_eq!(outcome["exit"], 1);
}

#[test]
fn output_that_is_not_json_leaves_the_json_field_empty() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({}));
    let fx = drive(&mut m, &mut ctx, &mut state, turn("harness.data", "x"));
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call,
            observation: report(0, "plain words"),
        },
    );
    assert_eq!(m.outcome().expect("outcome")["verdict"], Value::Null);
}

#[test]
fn on_failure_return_ends_a_failed_attempt_with_its_outputs_not_with_a_failure() {
    let doc = r#"
api_version: "2"
name: returning
on_failure: return
execution: { limits: { model_turns: 10 } }
gates:
  test: { command: [sh, t.sh] }
workflow:
  - name: work
    task: Do it.
    verify: [test]
outputs:
  said: { type: string, value: "{{ tasks.work.output }}" }
"#;
    let (mut m, mut ctx, mut state) = start(doc, json!({}));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.returning", "I did it"),
    );
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call,
            observation: report(2, ""),
        },
    );
    assert_eq!(
        state,
        HarnessState::Completed,
        "the attempt ends by returning"
    );
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["status"], "failed");
    assert_eq!(
        outcome["said"], "I did it",
        "what the attempt produced is kept"
    );
    assert_eq!(outcome["failure"]["code"], "gate_failed");
    assert!(outcome["failure"]["message"]
        .as_str()
        .unwrap()
        .contains("check `test`"));
}
