// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: what a run does with four language gaps a port found. A skipped task
//! reads as an empty output with the stop `skipped`; the run decides, from a
//! number input, how many repair turns and follow-up sends to use out of the
//! bound the plan was unrolled to; an empty thinking-budget input means
//! unbounded; a gate's output limit reaches the gate tool.
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

const THREAD: &str = "harness.gaps";

const DOC: &str = r#"
api_version: "2"
name: gaps
inputs: { tries: number, more: number, pick: boolean, budget: json }
execution: { limits: { model_turns: 60 } }
gates:
  check: { command: [sh, check.sh], output_bytes: 65536 }
workflow:
  - name: optional
    when: inputs.pick
    task: Maybe.
  - name: work
    task: Do it.
    thinking: { budget: "{{ inputs.budget }}" }
    verify: [check]
    repair: { attempts: "{{ inputs.tries }}", at_most: 3 }
    follow_ups:
      - task: Again.
        max: "{{ inputs.more }}"
        at_most: 2
outputs:
  said: { type: string, value: "[{{ tasks.optional.output }}] {{ tasks.optional.stop }}" }
"#;

fn run(
    tries: u64,
    more: u64,
    pick: bool,
    budget: Value,
) -> (HarnessMachine, Context, HarnessState, Vec<Effect>) {
    let (m, ctx, state) = start(
        DOC,
        json!({ "tries": tries, "more": more, "pick": pick, "budget": budget }),
    );
    let first = m.step_effect().into_iter().collect::<Vec<_>>();
    (m, ctx, state, first)
}

fn llm(effects: &[Effect]) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => Some(request.clone()),
        _ => None,
    })
}

fn gate(effects: &[Effect]) -> Option<(ToolCallId, Value)> {
    effects.iter().find_map(|e| match e {
        Effect::CallTool { call_id, args, .. } => Some((*call_id, args.clone())),
        _ => None,
    })
}

/// Answers every model turn with text and every gate with a failure until the
/// run ends; returns how many model turns were asked and the gate's args.
fn run_failing(tries: u64, more: u64) -> (usize, Value) {
    let (mut m, mut ctx, mut state, mut fx) = run(tries, more, false, json!(null));
    let (mut turns, mut args) = (0, Value::Null);
    for _ in 0..200 {
        if state.is_terminal() {
            break;
        }
        if llm(&fx).is_some() {
            turns += 1;
            fx = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "done"));
        } else if let Some((call_id, a)) = gate(&fx) {
            args = a;
            fx = drive(
                &mut m,
                &mut ctx,
                &mut state,
                Event::ToolSucceeded {
                    call_id,
                    observation: json!({"exit_code": 1, "timed_out": false, "error": null, "stdout": "no", "stderr": ""})
                        .to_string()
                        .into(),
                },
            );
        } else {
            fx = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
        }
    }
    assert_eq!(state, HarnessState::Failed, "the check never passes");
    (turns, args)
}

#[test]
fn a_skipped_task_reads_as_an_empty_output_and_the_stop_skipped() {
    let (m, ctx, _state, fx) = run(1, 1, false, json!(null));
    // The first model turn is the second task's: the first was skipped.
    assert!(llm(&fx).is_some(), "{fx:?}");
    let facts = sven_harness_runtime::HarnessRunState::load(&ctx).facts;
    assert_eq!(facts["task_optional"], json!(""));
    assert_eq!(facts["stop_optional"], json!("skipped"));
    drop(m);
}

#[test]
fn the_run_chooses_how_many_repairs_out_of_the_bound() {
    // A first turn, then one per repair the input allows, at most the 3 the
    // plan was unrolled to; with `more: 0` no follow-up is sent.
    for (tries, turns) in [(0, 1), (1, 2), (2, 3), (3, 4), (9, 4)] {
        let (asked, _) = run_failing(tries, 0);
        assert_eq!(asked, turns, "tries {tries}");
    }
}

#[test]
fn the_run_chooses_how_many_follow_up_sends_out_of_the_bound() {
    let (none, _) = run_failing(0, 0);
    let (one, _) = run_failing(0, 1);
    let (two, _) = run_failing(0, 2);
    assert_eq!((one - none, two - one), (1, 1), "{none} {one} {two}");
}

#[test]
fn a_gate_is_asked_to_keep_the_output_its_setting_names() {
    let (_, args) = run_failing(0, 0);
    assert_eq!(args["tail_bytes"], 65536);
}

#[test]
fn an_empty_thinking_budget_input_means_unbounded() {
    for (budget, wanted) in [
        (json!(256), Some(256)),
        (json!(null), None),
        (json!(""), None),
    ] {
        let (_m, _ctx, _state, fx) = run(1, 1, false, budget.clone());
        let request = llm(&fx).unwrap_or_else(|| panic!("a model turn: {fx:?}"));
        assert_eq!(
            request["decoding"]["thinking_budget"].as_u64(),
            wanted,
            "{budget}"
        );
    }
}
