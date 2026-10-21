// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: in a loop with `on_item_failure`, an item whose check fails for good
//! is recorded `failed`, the rollback checks run, and the loop goes on with
//! the next item; `success.work_items.all_settled` then holds while
//! `all_completed` would not. Driven at the kernel boundary.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessState};
use sven_hsm::{Context, Effect, Event, Machine, ToolCallId};

fn workflow(success: &str) -> String {
    format!(
        r#"
api_version: "2"
name: sweep
inputs: {{ files: json }}
execution: {{ limits: {{ model_turns: 20, tool_calls: 20 }} }}
gates:
  ok: {{ command: [check, "{{{{ item_id }}}}"] }}
  rollback: {{ command: [git, reset, --hard] }}
workflow:
  - name: files
    for_each:
      source: {{ input: files }}
      on_item_failure: {{ verify: [rollback] }}
    do:
      - name: fix
        task: "Fix {{{{ item }}}}."
        verify: [ok]
success:
  work_items: {{ {success}: true }}
outputs:
  done: {{ type: string, value: "{{{{ tasks.fix.output }}}}" }}
"#
    )
}

fn begin(yaml: &str) -> (HarnessMachine, Context, HarnessState) {
    let run = HarnessRunData::new(yaml, json!({ "files": ["a", "b"] })).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state)
}

fn gate(effects: &[Effect]) -> (ToolCallId, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallTool { call_id, args, .. } => Some((*call_id, args.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a gate call in {effects:?}"))
}

fn exit(
    m: &mut HarnessMachine,
    c: &mut Context,
    s: &mut HarnessState,
    id: ToolCallId,
    code: i64,
) -> Vec<Effect> {
    drive(
        m,
        c,
        s,
        Event::ToolSucceeded {
            call_id: id,
            observation: json!({ "exit_code": code, "timed_out": false, "error": null,
                                 "stdout": "", "stderr": "" })
            .to_string()
            .into(),
        },
    )
}

fn argv(args: &Value) -> Value {
    args["steps"][0]["argv"].clone()
}

fn run_sweep(success: &str) -> Value {
    let (mut m, mut c, mut s) = begin(&workflow(success));
    // Item a: the check fails, no repairs: the item fails and is rolled back.
    let fx = drive(&mut m, &mut c, &mut s, turn("harness.sweep.fix#0", "tried"));
    let (id, args) = gate(&fx);
    assert_eq!(argv(&args), json!(["check", "a"]));
    let fx = exit(&mut m, &mut c, &mut s, id, 1);
    let (id, args) = gate(&fx);
    assert_eq!(
        argv(&args),
        json!(["git", "reset", "--hard"]),
        "the rollback runs"
    );
    let fx = exit(&mut m, &mut c, &mut s, id, 0);
    // Item b runs, in a fresh conversation, and passes.
    let next = fx
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .expect("the next item starts");
    assert_eq!(next["thread"], "harness.sweep.fix#1");
    let fx = drive(
        &mut m,
        &mut c,
        &mut s,
        turn("harness.sweep.fix#1", "fixed b"),
    );
    let (id, _) = gate(&fx);
    exit(&mut m, &mut c, &mut s, id, 0);
    m.outcome().expect("the run ended")
}

#[test]
fn a_failed_item_is_rolled_back_and_the_loop_goes_on() {
    let outcome = run_sweep("all_settled");
    assert_eq!(outcome, json!({ "done": "fixed b" }));
}

#[test]
fn all_completed_does_not_hold_when_an_item_failed() {
    let outcome = run_sweep("all_completed");
    assert_eq!(outcome["code"], "success_check_failed", "{outcome}");
}
