// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an `iterate` step runs its body once per item of a collection it
//! froze on the first visit, records each item's completion in the run state,
//! and a resumed run carries on at the item it was on: completed items are
//! not run again and the collection is not read again. Driven at the kernel
//! boundary like the other machine tests, with a responder standing in for
//! the model and the tools.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessState, INTERRUPTED_OPERATION, RUN_STATE_FACT};
use sven_hsm::{Context, Effect, Event};

const THREAD: &str = "harness.looped";

fn workflow(source: &str, extra: &str) -> String {
    format!(
        r#"
api_version: "2"
name: looped
inputs: {{ modules: json }}
execution: {{ limits: {{ model_turns: 1000, tool_calls: 1000, steps: 100000 }} }}
gates:
  check: {{ command: [sh, check.sh, "{{{{ item_id }}}}"] }}
workflow:
  - name: modules
    for_each: {{ source: {source}, id: name }}
    task: "Harden {{{{ item.name }}}}."
    verify: [check]
{extra}
success:
  work_items: {{ all_completed: true }}
"#
    )
}

fn input_workflow() -> String {
    workflow("{ input: modules }", "")
}

fn modules(n: usize) -> Value {
    json!((0..n)
        .map(|i| json!({ "name": format!("m{i:03}"), "size": i }))
        .collect::<Vec<_>>())
}

/// What the responder saw.
#[derive(Default)]
struct Seen {
    /// The item named in each model prompt, in order.
    prompts: Vec<String>,
    gate_calls: usize,
    snapshot_calls: usize,
}

/// Answers every model turn and tool call until the run rests terminal, or
/// until `stop` returns true for an effect (which is not answered).
fn pump(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    state: &mut HarnessState,
    mut effects: Vec<Effect>,
    seen: &mut Seen,
    snapshot: Option<&Value>,
    stop: &dyn Fn(&Seen) -> bool,
) -> Vec<Effect> {
    for _ in 0..100_000 {
        if state.is_terminal() || stop(seen) {
            return effects;
        }
        let Some(effect) = effects
            .iter()
            .find(|e| matches!(e, Effect::CallLlm { .. } | Effect::CallTool { .. }))
        else {
            // Nothing to answer: tick the walk.
            effects = drive(m, ctx, state, Event::user_message("tick"));
            continue;
        };
        let event = match effect {
            Effect::CallLlm { request } => {
                seen.prompts.push(
                    request["instruction"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
                turn(THREAD, "hardened")
            }
            Effect::CallTool { call_id, name, .. } if name == "shell_gate" => {
                seen.gate_calls += 1;
                Event::ToolSucceeded {
                    call_id: *call_id,
                    observation: json!({"exit_code": 0, "timed_out": false, "error": null})
                        .to_string()
                        .into(),
                }
            }
            Effect::CallTool {
                call_id,
                name,
                args,
                ..
            } if name == "read_json_snapshot" => {
                seen.snapshot_calls += 1;
                assert_eq!(args["pointer"], "/items");
                Event::ToolSucceeded {
                    call_id: *call_id,
                    observation: json!({ "digest": "d1", "value": snapshot.cloned().expect("a snapshot to serve") })
                        .to_string()
                        .into(),
                }
            }
            other => panic!("unexpected effect {other:?}"),
        };
        effects = drive(m, ctx, state, event);
    }
    panic!("the run did not settle");
}

fn run_state(ctx: &Context) -> Value {
    ctx.fact(RUN_STATE_FACT).cloned().expect("run state")
}

fn begin(yaml: &str, inputs: Value) -> (HarnessMachine, Context, HarnessState, Vec<Effect>) {
    let (m, ctx, state) = start(yaml, inputs);
    let first = m.step_effect().into_iter().collect();
    (m, ctx, state, first)
}

#[test]
fn every_item_of_the_frozen_collection_runs_once_in_order() {
    let (mut m, mut ctx, mut state, fx) =
        begin(&input_workflow(), json!({ "modules": modules(5) }));
    let mut seen = Seen::default();
    pump(&mut m, &mut ctx, &mut state, fx, &mut seen, None, &|_| {
        false
    });
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(seen.prompts.len(), 5);
    for (i, prompt) in seen.prompts.iter().enumerate() {
        assert!(prompt.contains(&format!("m{i:03}")), "{prompt}");
    }
    assert_eq!(seen.gate_calls, 5, "each item's check ran");
    let summary = &run_state(&ctx)["facts"]["work_modules"];
    assert_eq!(
        summary,
        &json!({ "total": 5, "completed": 5, "pending": 0 })
    );
    let items = &run_state(&ctx)["iterations"]["modules__iter"]["items"];
    assert!(items
        .as_array()
        .expect("items")
        .iter()
        .all(|i| i["status"] == "completed"
            && i["verified_under"].as_str().is_some_and(|d| d.len() == 64)));
}

#[test]
fn an_empty_collection_finishes_the_loop_without_running_the_body() {
    let (mut m, mut ctx, mut state, fx) = begin(&input_workflow(), json!({ "modules": [] }));
    let mut seen = Seen::default();
    pump(&mut m, &mut ctx, &mut state, fx, &mut seen, None, &|_| {
        false
    });
    assert_eq!(state, HarnessState::Completed);
    assert!(seen.prompts.is_empty());
}

#[test]
fn a_bad_collection_fails_the_run_with_a_code_that_says_why() {
    let cases = [
        ("not an array", json!({"a": 1}), "iterate_source_not_array"),
        (
            "a missing id",
            json!([{ "name": "a" }, { "size": 2 }]),
            "iterate_missing_id",
        ),
        ("an empty id", json!([{ "name": "" }]), "iterate_missing_id"),
        (
            "an id that is an object",
            json!([{ "name": { "x": 1 } }]),
            "iterate_missing_id",
        ),
        (
            "a repeated id",
            json!([{ "name": "a" }, { "name": "a" }]),
            "iterate_duplicate_id",
        ),
        ("too many items", modules(501), "iterate_too_many_items"),
    ];
    for (what, modules, code) in cases {
        let (mut m, mut ctx, mut state, fx) =
            begin(&input_workflow(), json!({ "modules": modules }));
        let mut seen = Seen::default();
        pump(&mut m, &mut ctx, &mut state, fx, &mut seen, None, &|_| {
            false
        });
        assert_eq!(state, HarnessState::Failed, "{what}");
        assert_eq!(m.outcome().expect("outcome")["code"], code, "{what}");
        assert!(
            seen.prompts.is_empty(),
            "{what}: no item runs from a bad collection"
        );
    }
}

#[test]
fn backlog_mutation_does_not_skip_items() {
    // The collection is a file the agent writes. It is read once; whatever the
    // agent does to the file afterwards cannot add, drop or reorder an item.
    let yaml = workflow("{ json: { file: backlog.json, pointer: /items } }", "");
    let (mut m, mut ctx, mut state, fx) = begin(&yaml, json!({ "modules": [] }));
    let call = fx.iter().find_map(|e| match e {
        Effect::CallTool { name, args, .. } => Some((name.clone(), args.clone())),
        _ => None,
    });
    let (name, args) = call.expect("the first visit reads the collection");
    assert_eq!(name, "read_json_snapshot");
    assert_eq!(args, json!({ "path": "backlog.json", "pointer": "/items" }));

    let mut seen = Seen::default();
    // The agent's turns would rewrite the file; the responder serves the
    // snapshot only on request and counts requests.
    pump(
        &mut m,
        &mut ctx,
        &mut state,
        fx,
        &mut seen,
        Some(&modules(4)),
        &|_| false,
    );
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(seen.snapshot_calls, 1, "the collection is read once");
    assert_eq!(seen.prompts.len(), 4);
    let frozen = &run_state(&ctx)["iterations"]["modules__iter"];
    assert_eq!(frozen["source_digest"], "d1");
    assert_eq!(frozen["items"].as_array().map(Vec::len), Some(4));
}

#[test]
fn an_unreadable_collection_fails_the_run() {
    let yaml = workflow("{ json: { file: backlog.json, pointer: /items } }", "");
    let (mut m, mut ctx, mut state, fx) = begin(&yaml, json!({ "modules": [] }));
    let call_id = fx.iter().find_map(|e| match e {
        Effect::CallTool { call_id, .. } => Some(*call_id),
        _ => None,
    });
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: call_id.expect("call"),
            error: "no such file".into(),
        },
    );
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["code"], "iterate_source_unreadable");
    assert!(outcome["message"]
        .as_str()
        .is_some_and(|m| m.contains("no such file")));
}

#[test]
fn a_failed_check_on_one_item_stops_the_loop_there() {
    let (mut m, mut ctx, mut state, fx) =
        begin(&input_workflow(), json!({ "modules": modules(3) }));
    let mut seen = Seen::default();
    // Answer item 0 normally, then fail item 1's check.
    let fx = pump(&mut m, &mut ctx, &mut state, fx, &mut seen, None, &|s| {
        s.prompts.len() == 2 && s.gate_calls == 1
    });
    let call_id = fx.iter().find_map(|e| match e {
        Effect::CallTool { call_id, .. } => Some(*call_id),
        _ => None,
    });
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call_id.expect("item 1's gate call"),
            observation:
                json!({"exit_code": 2, "timed_out": false, "error": null, "stderr": "boom"})
                    .to_string()
                    .into(),
        },
    );
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["code"], "gate_failed");
    assert!(
        outcome["message"]
            .as_str()
            .is_some_and(|m| m.contains("m001")),
        "names the item: {outcome}"
    );
    let items = &run_state(&ctx)["iterations"]["modules__iter"]["items"];
    assert_eq!(items[0]["status"], "completed");
    assert_eq!(
        items[1]["status"], "active",
        "the failed item is not marked completed"
    );
    assert_eq!(items[2]["status"], "pending");
}

#[test]
fn a_hundred_items_interrupted_midway_resume_without_redoing_completed_ones() {
    let yaml = input_workflow();
    let inputs = json!({ "modules": modules(100) });
    let (mut m, mut ctx, mut state, fx) = begin(&yaml, inputs);
    let mut seen = Seen::default();

    // Run until item 37's model turn is in flight: items 0 to 36 are complete.
    let stop_at = |s: &Seen| s.prompts.len() == 37 && s.gate_calls == 37;
    let _ = pump(&mut m, &mut ctx, &mut state, fx, &mut seen, None, &stop_at);
    assert_eq!(seen.prompts.len(), 37);

    // The process dies. A new one restores the checkpoint; the host settles
    // what was in flight as interrupted.
    let snapshot = m.snapshot(&ctx);
    drop((m, ctx));
    let (mut m, mut ctx) =
        HarnessMachine::restore(yaml, snapshot).expect("the checkpoint restores");
    let mut state = HarnessState::WaitingForModel;
    assert_eq!(run_state(&ctx)["iterations"]["modules__iter"]["active"], 37);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::LlmFailed {
            error: INTERRUPTED_OPERATION.to_string(),
        },
    );
    let mut resumed = Seen::default();
    pump(
        &mut m,
        &mut ctx,
        &mut state,
        fx,
        &mut resumed,
        None,
        &|_| false,
    );

    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        resumed.prompts.len(),
        63,
        "items 37 to 99 run; 0 to 36 do not run again"
    );
    assert!(
        resumed.prompts[0].contains("m037"),
        "{}",
        resumed.prompts[0]
    );
    assert_eq!(
        resumed.prompts.last().map(|p| p.contains("m099")),
        Some(true)
    );
    assert_eq!(
        seen.gate_calls + resumed.gate_calls,
        100,
        "every item is checked exactly once"
    );
    let items = &run_state(&ctx)["iterations"]["modules__iter"]["items"];
    assert!(items
        .as_array()
        .expect("items")
        .iter()
        .all(|i| i["status"] == "completed"));
    assert_eq!(
        items[37]["attempts"], 1,
        "the interrupted turn was run again, not the item restarted"
    );
    assert_eq!(
        run_state(&ctx)["facts"]["work_modules"],
        json!({ "total": 100, "completed": 100, "pending": 0 })
    );
}

#[test]
fn an_until_loop_repeats_its_body_until_the_condition_leaves_it() {
    let yaml = r#"
api_version: "2"
name: looped
execution: { limits: { model_turns: 100 } }
gates:
  coverage:
    command: [sh, cov.sh]
    metrics: { line_pct: { file: cov.json, pointer: /line_pct, type: number } }
workflow:
  - name: cover
    until: { metric: coverage.line_pct, gte: 90, max_iterations: 3 }
    task: "Raise coverage (pass {{ iteration }})."
"#;
    // The metric never reaches the threshold: every pass is counted, then the
    // loop is exhausted and the run fails.
    let (mut m, mut ctx, mut state) = start(yaml, json!({}));
    let mut passes = Vec::new();
    for pass in 1..=3 {
        let effect = m.step_effect().expect("a model turn");
        let Effect::CallLlm { request } = effect else {
            panic!("model turn expected")
        };
        passes.push(
            request["instruction"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        let fx = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "tried"));
        // The condition: a fresh run of the command, then the number's check.
        let Some(Effect::CallTool { call_id, name, .. }) = fx
            .iter()
            .find(|e| matches!(e, Effect::CallTool { .. }))
            .cloned()
        else {
            panic!("pass {pass}: the metric's command runs after the body: {fx:?}");
        };
        assert_eq!(name, "shell_gate");
        let fx = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::ToolSucceeded {
                call_id,
                observation: json!({"exit_code": 0, "timed_out": false, "error": null})
                    .to_string()
                    .into(),
            },
        );
        // The verifier reads the file; its verdict is that the number is too low.
        assert!(
            fx.iter().any(|e| matches!(e, Effect::Verify { .. })),
            "pass {pass}: {fx:?}"
        );
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::VerificationComplete {
                verdict: sven_vocab::verify::VerifierVerdict::Failed {
                    reason: "line_pct = 40, op Gte 90 failed".into(),
                },
            },
        );
    }
    assert!(
        passes[0].contains("pass 1") && passes[2].contains("pass 3"),
        "{passes:?}"
    );
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("outcome");
    assert_eq!(outcome["code"], "until_exhausted");
    assert!(outcome["message"]
        .as_str()
        .is_some_and(|m| m.contains("coverage.line_pct")));
}

#[test]
fn a_file_check_names_the_item_it_belongs_to() {
    let yaml = workflow(
        "{ input: modules }",
        "    outputs: [\"reports/{{ item_id }}.md\"]",
    );
    let (mut m, mut ctx, mut state, _) = begin(&yaml, json!({ "modules": modules(2) }));
    let fx = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "hardened"));
    let Some(Effect::Verify { spec }) = fx.into_iter().find(|e| matches!(e, Effect::Verify { .. }))
    else {
        panic!("the item's output is verified before its checks run");
    };
    let spec = serde_json::to_value(&spec).expect("spec");
    assert_eq!(spec["path"], "reports/m000.md");
}
