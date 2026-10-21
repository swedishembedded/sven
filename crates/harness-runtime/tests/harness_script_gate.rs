// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a script gate calls the gate tool with its steps and variables, all
//! templates resolved; the machine stores which step ended the gate and what
//! each step cost, and a repair prompt (or a later template) can say so.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning build and test
//! scripts into trusted checks then you can procure our services by sending
//! an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_hsm::{Effect, Event};

const DOC: &str = r#"
api_version: "2"
name: scripted
inputs: { target: string }
execution: { limits: { model_turns: 10 } }
gates:
  build:
    run:
      - { name: compile, script: "make {{ inputs.target }}" }
      - { name: test, script: "make check" }
    env: { TARGET: "{{ inputs.target }}" }
workflow:
  - name: work
    task: Do it.
    verify: [build]
    repair:
      attempts: 1
      prompt: "{{ failure.failed_step }} failed in {{ failure.gate }}: {{ failure.detail }}"
outputs:
  failed: { type: string, value: "{{ gates.build.failed_step }}" }
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

fn report(failed_step: Value, exit: i64) -> Value {
    json!({
        "exit_code": exit, "timed_out": false, "error": null, "stdout": "", "stderr": "boom",
        "failed_step": failed_step,
        "steps": [{"name": "compile", "exit_code": 0, "duration_ms": 3}],
    })
    .to_string()
    .into()
}

#[test]
fn the_tool_is_called_with_resolved_steps_and_variables() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "target": "all" }));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.scripted", "done"),
    );
    let (_, args) = gate_call(&fx);
    assert_eq!(
        args["steps"],
        json!([
            { "name": "compile", "argv": ["bash", "-eo", "pipefail", "-c", "make all"] },
            { "name": "test", "argv": ["bash", "-eo", "pipefail", "-c", "make check"] },
        ])
    );
    assert_eq!(args["env"], json!({ "TARGET": "all" }));
}

#[test]
fn the_failed_step_is_stored_and_read_by_the_repair_prompt() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "target": "all" }));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.scripted", "done"),
    );
    let (call, _) = gate_call(&fx);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call,
            observation: report(json!("test"), 2),
        },
    );
    let prompt = fx
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.to_string()),
            _ => None,
        })
        .expect("a repair turn");
    assert!(
        prompt.contains("test failed in build: the command exited with status 2 (step `test`)"),
        "{prompt}"
    );
}

#[test]
fn a_gate_that_passed_has_no_failed_step() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "target": "all" }));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.scripted", "done"),
    );
    let (call, _) = gate_call(&fx);
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call,
            observation: report(Value::Null, 0),
        },
    );
    assert_eq!(m.outcome().expect("outcome")["failed"], Value::Null);
}
