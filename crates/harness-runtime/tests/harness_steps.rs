// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The schema'd `predict` and `delegate` steps' spec: the same
//! kernel-boundary drivers as `harness_machine.rs`, for the steps whose
//! contracts arrived after the core walk.

mod common;

use common::{drive, start, turn};
use sven_harness_runtime::{HarnessState, RUN_STATE_FACT};
use sven_hsm::Effect;

const SCHEMA_HARNESS: &str = r#"
api_version: "1"
name: shaped
entry: ask
limits: { max_steps: 20, max_model_turns: 5 }
schemas:
  answer:
    type: object
    properties:
      greeting: { type: string }
    required: [greeting]
steps:
  ask:
    predict:
      prompt: "Say hello to {{ inputs.name }}"
      schema: answer
      store_as: spoken
      then: done
  done:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

#[test]
fn a_predict_step_with_a_schema_constrains_the_turn() {
    let (mut m, mut ctx, mut state) = start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
    let ask = m.step_effect().expect("effect");
    let Effect::CallLlm { request } = &ask else {
        panic!("predict emits CallLlm, got {ask:?}");
    };
    assert_eq!(request["schema_name"], "answer");
    assert_eq!(request["schema"]["type"], "object");
    assert_eq!(request["schema"]["required"][0], "greeting");

    // A conforming JSON answer is parsed and stored as the object.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.shaped", "{\"greeting\": \"hi Ada\"}"),
    );
    assert!(
        fx.is_empty(),
        "storing the answer advances by internal event only"
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("ask"))
            .map(|s| s["value"].clone())),
        Some(serde_json::json!({"greeting": "hi Ada"}))
    );
}

#[test]
fn a_non_json_answer_to_a_schema_turn_fails_the_step() {
    let (mut m, mut ctx, mut state) = start(SCHEMA_HARNESS, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("effect");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.shaped", "not json"),
    );
    assert_eq!(state, HarnessState::Failed);
    let out = m.outcome().expect("outcome");
    assert_eq!(out["code"], "invalid_answer");
    let _ = fx;
}
