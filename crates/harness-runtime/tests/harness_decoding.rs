// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the sampling and thinking settings a step declares travel with every
//! model turn the step makes, the first and each continuation of an agent
//! loop, and a model_only step is a single toolless turn whose answer is
//! stored for later steps.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_hsm::{Effect, Event, ProposedToolCall, ToolCallId, ToolCapability};

const DOC: &str = r#"
api_version: "2"
name: decoded
agent: { profile: coding, sampling: { temperature: 0.5 }, thinking: { enabled: true, budget: 512 } }
execution: { limits: { model_turns: 10, tool_calls: 10 } }
workflow:
  - name: plan
    model_only: true
    schema: { type: object, required: [goal], properties: { goal: { type: string } } }
    sampling: { temperature: 0, seed: 11 }
    thinking: { enabled: false }
    task: Plan.
  - name: work
    task: "Do {{ tasks.plan.output.goal }}."
    tools: [read_file]
outputs:
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

fn request(effect: &Effect) -> Value {
    let Effect::CallLlm { request } = effect else {
        panic!("a model call: {effect:?}")
    };
    request.clone()
}

#[test]
fn settings_ride_every_turn_and_a_model_only_answer_feeds_the_next_step() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({}));
    // The model-only step: no tools, a schema, its own settings.
    let first = request(&m.step_effect().expect("effect"));
    assert_eq!(first["tools"], json!([]));
    assert_eq!(first["schema_name"], "plan");
    assert_eq!(
        first["decoding"],
        json!({ "temperature": 0.0, "seed": 11, "thinking": false, "thinking_budget": 512 })
    );

    // Its structured answer is stored and a field of it templates the next prompt.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.decoded", r#"{"goal":"ship"}"#),
    );
    let work = request(
        fx.iter()
            .find(|e| matches!(e, Effect::CallLlm { .. }))
            .expect("worker turn"),
    );
    assert_eq!(work["instruction"], "Do ship.");
    assert_eq!(
        work["decoding"],
        json!({ "temperature": 0.5, "thinking": true, "thinking_budget": 512 })
    );

    // The agent loop's continuation after a tool call carries them too.
    let proposal = Event::LlmTurnComplete {
        thread: "harness.decoded".into(),
        text: String::new(),
        tool_calls: vec![ProposedToolCall {
            call_id: ToolCallId::new(),
            name: "read_file".into(),
            capability: ToolCapability::ReadFile,
            args: json!({ "path": "a" }),
        }],
    };
    let fx = drive(&mut m, &mut ctx, &mut state, proposal);
    let call = fx.iter().find_map(|e| match e {
        Effect::CallTool { call_id, .. } => Some(*call_id),
        _ => None,
    });
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call.expect("the tool call"),
            observation: "contents".into(),
        },
    );
    let next = request(
        fx.iter()
            .find(|e| matches!(e, Effect::CallLlm { .. }))
            .expect("continuation"),
    );
    assert_eq!(next["decoding"], work["decoding"]);
}

#[test]
fn a_model_only_answer_that_keeps_breaking_its_schema_fails_the_run() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({}));
    // The first answer and the two default repairs.
    for _ in 0..3 {
        let _ = drive(
            &mut m,
            &mut ctx,
            &mut state,
            turn("harness.decoded", "not json at all"),
        );
    }
    assert_eq!(m.outcome().expect("outcome")["code"], "invalid_answer");
}

#[test]
fn substituted_values_are_data_and_never_template_syntax() {
    let doc = r#"
api_version: "2"
name: data
inputs: { diff: string }
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: "Apply this diff:\n{{ inputs.diff }}\nEnd."
"#;
    let diff = "+ let a = \"{{ inputs.diff }}\";\n+ {{ unknown }} {% if x %} }} {{";
    let (m, _ctx, _state) = start(doc, json!({ "diff": diff }));
    let asked = request(&m.step_effect().expect("effect"));
    assert_eq!(
        asked["instruction"],
        format!("Apply this diff:\n{diff}\nEnd."),
        "braces in a substituted value are carried verbatim"
    );
}

const FROM_INPUTS: &str = r#"
api_version: "2"
name: seeded
inputs: { seed: number, cap: number, think: boolean }
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    model_only: true
    sampling: { seed: "{{ inputs.seed }}" }
    thinking: { enabled: "{{ inputs.think }}" }
    turn: { max_output_tokens: "{{ inputs.cap }}", first_response_timeout: 120 }
    task: Go.
"#;

#[test]
fn settings_that_name_an_input_take_the_value_the_run_was_started_with() {
    let (m, _, _) = start(
        FROM_INPUTS,
        json!({ "seed": 42, "cap": 777, "think": false }),
    );
    let asked = request(&m.step_effect().expect("effect"));
    assert_eq!(
        asked["decoding"],
        json!({ "seed": 42, "thinking": false, "max_output_tokens": 777, "first_response_timeout": 120 })
    );
}

#[test]
fn a_setting_that_does_not_resolve_fails_the_run_and_names_it() {
    // A missing input, and a value of the wrong kind.
    for inputs in [
        json!({ "cap": 1, "think": true }),
        json!({ "seed": "x", "cap": 1, "think": true }),
    ] {
        let (m, _, state) = start(FROM_INPUTS, inputs);
        assert_eq!(state, sven_harness_runtime::HarnessState::Failed);
        let outcome = m.outcome().expect("outcome");
        assert_eq!(outcome["code"], "invalid_setting");
        assert!(
            outcome["message"]
                .as_str()
                .is_some_and(|m| m.contains("`seed`")),
            "{outcome}"
        );
    }
}
