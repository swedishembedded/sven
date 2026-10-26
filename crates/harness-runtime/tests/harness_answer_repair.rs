// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a schema'd model-only answer that is truncated or does not parse
//! earns bounded repair turns (the task's `repair.attempts`, else 2) in the
//! same conversation, each naming the problem. Once they are spent the
//! failure is the item's inside a `for_each` with `on_item_failure`, and the
//! run's otherwise. Driven at the kernel boundary.
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

fn workflow(repair: &str, loops: bool) -> String {
    let each = if loops {
        "    for_each:\n      source: { input: files }\n      on_item_failure: { verify: [] }\n"
    } else {
        ""
    };
    let body = format!(
        "    model_only: true\n    task: \"Describe {{{{ inputs.what }}}}.\"\n{repair}    schema:\n      type: object\n      properties: {{ title: {{ type: string }} }}\n      required: [title]\n"
    );
    let task = if loops {
        format!(
            "  - name: files\n{each}    do:\n      - name: describe\n{}",
            body.replace("\n    ", "\n        ")
                .replacen("    ", "        ", 1)
        )
    } else {
        format!("  - name: describe\n{body}")
    };
    format!(
        "api_version: \"2\"\nname: shaped\ninputs: {{ what: string, files: json }}\n\
         execution: {{ limits: {{ model_turns: 30 }} }}\nworkflow:\n{task}"
    )
}

fn request(effects: &[Effect]) -> Option<Value> {
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => Some(request.clone()),
        _ => None,
    })
}

fn answer(
    m: &mut sven_harness_runtime::HarnessMachine,
    ctx: &mut sven_hsm::Context,
    state: &mut HarnessState,
    thread: &str,
    text: &str,
) -> Vec<Effect> {
    drive(m, ctx, state, turn(thread, text))
}

fn begin(
    yaml: &str,
) -> (
    sven_harness_runtime::HarnessMachine,
    sven_hsm::Context,
    HarnessState,
    Value,
) {
    let (m, ctx, state) = start(yaml, json!({ "what": "it", "files": ["a", "b"] }));
    let first = match m.step_effect().expect("the first turn") {
        Effect::CallLlm { request } => request,
        other => panic!("{other:?}"),
    };
    (m, ctx, state, first)
}

#[test]
fn an_unparseable_answer_is_repaired_in_the_same_conversation() {
    let (mut m, mut ctx, mut state, first) = begin(&workflow("", false));
    let thread = first["thread"].as_str().unwrap().to_string();

    let fx = answer(&mut m, &mut ctx, &mut state, &thread, "no json here");
    let repair = request(&fx).expect("a repair turn");
    assert_eq!(repair["thread"], first["thread"]);
    assert_eq!(repair["origin"], "repair");
    assert_eq!(repair["schema_name"], first["schema_name"]);
    assert!(
        repair["instruction"]
            .as_str()
            .unwrap()
            .contains("not valid JSON"),
        "{repair}"
    );

    answer(
        &mut m,
        &mut ctx,
        &mut state,
        &thread,
        r#"{"title": "fixed"}"#,
    );
    assert_ne!(state, HarnessState::Failed, "{:?}", m.outcome());
}

#[test]
fn a_truncated_answer_is_repaired_and_says_so() {
    let (mut m, mut ctx, mut state, first) = begin(&workflow("", false));
    let thread = first["thread"].as_str().unwrap().to_string();
    drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::Internal(sven_hsm::event::InternalEvent::Custom {
            name: sven_hsm::TURN_TRUNCATED.to_string(),
            payload: Value::Null,
        }),
    );
    let fx = answer(&mut m, &mut ctx, &mut state, &thread, r#"{"title": "cut"#);
    let repair = request(&fx).expect("a repair turn");
    assert!(
        repair["instruction"].as_str().unwrap().contains("cut off"),
        "{repair}"
    );
}

#[test]
fn repairs_are_bounded_by_the_tasks_attempts_then_the_run_fails() {
    let (mut m, mut ctx, mut state, first) =
        begin(&workflow("    repair: { attempts: 1 }\n", false));
    let thread = first["thread"].as_str().unwrap().to_string();
    let fx = answer(&mut m, &mut ctx, &mut state, &thread, "bad");
    assert!(request(&fx).is_some(), "the one repair");
    let fx = answer(&mut m, &mut ctx, &mut state, &thread, "still bad");
    assert!(request(&fx).is_none(), "no second repair: {fx:?}");
    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().unwrap()["code"], "invalid_answer");
}

#[test]
fn an_answer_that_stays_unusable_fails_only_its_item_inside_a_loop() {
    let (mut m, mut ctx, mut state, first) = begin(&workflow("", true));
    let thread = first["thread"].as_str().unwrap().to_string();
    // The default two repairs, then the item fails and item b starts.
    let mut fx = answer(&mut m, &mut ctx, &mut state, &thread, "bad");
    assert_eq!(request(&fx).unwrap()["origin"], "repair");
    fx = answer(&mut m, &mut ctx, &mut state, &thread, "bad");
    assert_eq!(request(&fx).unwrap()["origin"], "repair");
    fx = answer(&mut m, &mut ctx, &mut state, &thread, "bad");
    let next = request(&fx).expect("the next item's turn");
    assert_eq!(next["origin"], "task", "{next}");
    assert_ne!(next["thread"], first["thread"], "a conversation per item");
    assert_ne!(state, HarnessState::Failed, "{:?}", m.outcome());
}
