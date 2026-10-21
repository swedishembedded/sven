// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the supervisor correction loop. After the agent's result is judged by
//! a gate, a model-only author writes a message from the findings, a second
//! gate decides whether it may be sent (the author rewrites a refused one), the
//! accepted message is delivered as a user turn into the agent's own
//! conversation with a fresh allowance, and the result is judged again. Running
//! out of corrections or of rewrites ends the loop; it does not fail the run.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessState};
use sven_hsm::{Context, Effect, Event, ToolCallId};

const DOC: &str = r#"
api_version: "2"
name: corrected
inputs: { rounds: number, brief: string }
execution: { limits: { model_turns: 60 } }
gates:
  check: { command: [replay, check], timeout: 30 }
  accept: { command: [replay, accept], stdin: "{{ tasks.supervisor.output }}", timeout: 30 }
workflow:
  - name: work
    task: "{{ inputs.brief }}"
    allowance: 10
    corrections:
      max: 3
      count: "{{ inputs.rounds }}"
      check: check
      while: { all: ["!gates.check.passed", "gates.check.json.correctable"] }
      author:
        name: supervisor
        system: You are the supervisor.
        task: "The engineer changed: {{ gates.check.json.changes }}\nWrite the follow-up."
      accept: accept
      retries: 1
      allowance: 4
outputs:
  said: { type: string, value: "{{ tasks.work.output }}" }
  left: { type: number, value: "{{ tasks.work.calls_left }}" }
"#;

const AGENT: &str = "harness.corrected.work";
const SUPERVISOR: &str = "harness.corrected.supervisor";

fn report(exit: i64, stdout: &str) -> Value {
    json!({ "exit_code": exit, "timed_out": false, "error": null, "stdout": stdout, "stderr": "" })
        .to_string()
        .into()
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

fn llm(effects: &[Effect]) -> Value {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a model call in {effects:?}"))
}

fn answer(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    s: &mut HarnessState,
    id: ToolCallId,
    exit: i64,
    out: &str,
) -> Vec<Effect> {
    drive(
        m,
        ctx,
        s,
        Event::ToolSucceeded {
            call_id: id,
            observation: report(exit, out),
        },
    )
}

const FAILING: &str = r#"{"correctable":true,"changes":"edited a.c"}"#;

#[test]
fn a_failed_check_earns_a_message_from_the_supervisor_delivered_to_the_agent() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "rounds": 2, "brief": "Fix it." }));
    // The agent works; its result is judged.
    let fx = drive(&mut m, &mut ctx, &mut state, turn(AGENT, "I changed a.c"));
    let (check, args) = gate(&fx);
    assert_eq!(args["steps"][0]["argv"], json!(["replay", "check"]));
    // It failed in a correctable way: the supervisor is asked, in its own
    // conversation, with the findings in the prompt.
    let fx = answer(&mut m, &mut ctx, &mut state, check, 1, FAILING);
    let ask = llm(&fx);
    assert_eq!(ask["thread"], SUPERVISOR);
    assert_eq!(ask["system"], "You are the supervisor.");
    assert_eq!(ask["tools"], json!([]));
    assert!(
        ask["instruction"].as_str().unwrap().contains("edited a.c"),
        "{ask}"
    );

    // The message goes through the accept gate, on standard input.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(SUPERVISOR, "Please look at b.c too."),
    );
    let (accept, args) = gate(&fx);
    assert_eq!(args["steps"][0]["argv"], json!(["replay", "accept"]));
    assert_eq!(args["stdin"], "Please look at b.c too.");

    // Accepted: it is a user turn in the AGENT's conversation, labelled as a
    // correction, with a fresh allowance.
    let fx = answer(&mut m, &mut ctx, &mut state, accept, 0, "");
    let sent = llm(&fx);
    assert_eq!(sent["thread"], AGENT);
    assert_eq!(sent["instruction"], "Please look at b.c too.");
    assert_eq!(sent["origin"], "correction");

    // The agent answers; the result is judged again, and this time it passes.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(AGENT, "Done, b.c as well"),
    );
    let (recheck, _) = gate(&fx);
    let _ = answer(
        &mut m,
        &mut ctx,
        &mut state,
        recheck,
        0,
        r#"{"correctable":false}"#,
    );
    assert_eq!(state, HarnessState::Completed);
    let outcome = m.outcome().unwrap();
    assert_eq!(outcome["said"], "Done, b.c as well");
    assert_eq!(
        outcome["left"], 4,
        "the allowance was set again for the correction"
    );
}

#[test]
fn a_refused_message_is_rewritten_and_when_the_rewrites_are_spent_the_loop_ends_without_failing() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "rounds": 2, "brief": "Fix it." }));
    let fx = drive(&mut m, &mut ctx, &mut state, turn(AGENT, "done"));
    let (check, _) = gate(&fx);
    let _ = answer(&mut m, &mut ctx, &mut state, check, 1, FAILING);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(SUPERVISOR, "The diff says line 3."),
    );
    let (accept, _) = gate(&fx);
    // Refused once: the author is asked again in its own conversation, shown why.
    let fx = answer(&mut m, &mut ctx, &mut state, accept, 1, "mentions the diff");
    let again = llm(&fx);
    assert_eq!(again["thread"], SUPERVISOR);
    assert!(
        again["instruction"]
            .as_str()
            .unwrap()
            .contains("mentions the diff"),
        "{again}"
    );
    // Refused again: no rewrites left, so no correction is sent and the loop ends.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn(SUPERVISOR, "Line 3 of the diff."),
    );
    let (accept, _) = gate(&fx);
    let _ = answer(&mut m, &mut ctx, &mut state, accept, 1, "still");
    assert_eq!(
        state,
        HarnessState::Completed,
        "an exhausted correction is not a failure"
    );
}

#[test]
fn the_corrections_end_when_the_count_the_run_was_given_is_spent() {
    // One correction allowed by the input; the result stays bad.
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "rounds": 1, "brief": "Fix it." }));
    let fx = drive(&mut m, &mut ctx, &mut state, turn(AGENT, "done"));
    let (check, _) = gate(&fx);
    let _ = answer(&mut m, &mut ctx, &mut state, check, 1, FAILING);
    let fx = drive(&mut m, &mut ctx, &mut state, turn(SUPERVISOR, "Try again."));
    let (accept, _) = gate(&fx);
    let _ = answer(&mut m, &mut ctx, &mut state, accept, 0, "");
    let fx = drive(&mut m, &mut ctx, &mut state, turn(AGENT, "tried"));
    let (recheck, _) = gate(&fx);
    let _ = answer(&mut m, &mut ctx, &mut state, recheck, 1, FAILING);
    assert_eq!(
        state,
        HarnessState::Completed,
        "no second correction: the run went on"
    );
}

#[test]
fn a_result_that_is_not_correctable_gets_no_message() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({ "rounds": 2, "brief": "Fix it." }));
    let fx = drive(&mut m, &mut ctx, &mut state, turn(AGENT, "done"));
    let (check, _) = gate(&fx);
    let _ = answer(
        &mut m,
        &mut ctx,
        &mut state,
        check,
        1,
        r#"{"correctable":false}"#,
    );
    assert_eq!(state, HarnessState::Completed);
}
