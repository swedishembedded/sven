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
fn a_model_only_answer_that_breaks_its_schema_fails_the_run() {
    let (mut m, mut ctx, mut state) = start(DOC, json!({}));
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.decoded", "not json at all"),
    );
    assert_eq!(m.outcome().expect("outcome")["code"], "invalid_answer");
}
