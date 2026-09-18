// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the `predict` machine runs one schema-constrained, tool-free turn per
//! message.
//!
//! This is the execution strategy behind a typed model-driven method that only
//! has to produce a value - no investigation, no tools. It is a separate
//! machine rather than a mode of the conversational one because "the model may
//! not call anything" is a property of the turn, not a prompt request.
//!
//! Swedish Embedded AB implements deterministic agent execution strategies for
//! its clients. If your team needs expertise in constrained model execution
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde_json::json;
use sven_hsm::{Context, Effect, Event, Hsm};
use sven_machines::machines::predict::{PredictMachine, PredictState, RESULT_FACT, SCHEMA_FACT};

/// A machine seeded with a return schema, as the SDK seeds it.
fn seeded() -> (Hsm<PredictMachine>, Context) {
    let mut hsm = Hsm::new(PredictMachine::new());
    let mut ctx = Context::new();
    ctx.set_fact(
        SCHEMA_FACT,
        json!({"type": "object", "properties": {"risk": {"type": "integer"}}}),
    );
    ctx.set_fact("predict.schema_name", json!("assess"));
    hsm.init(&mut ctx);
    (hsm, ctx)
}

/// The single `CallLlm` request a dispatch produced.
fn turn_request(effects: &[Effect]) -> serde_json::Value {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .expect("the machine asked the model for something")
}

#[test]
fn a_message_starts_a_turn_carrying_the_seeded_schema() {
    let (mut hsm, mut ctx) = seeded();

    let out = hsm.dispatch(&Event::user_message("assess this change"), &mut ctx);

    let req = turn_request(&out.effects);
    assert_eq!(
        req["schema"]["properties"]["risk"]["type"], "integer",
        "the turn must carry the return type's schema so the provider can \
         constrain the response: {req}"
    );
    assert_eq!(req["schema_name"], "assess", "and name it: {req}");
}

#[test]
fn the_model_is_offered_no_tools() {
    let (mut hsm, mut ctx) = seeded();

    let out = hsm.dispatch(&Event::user_message("assess this change"), &mut ctx);

    let req = turn_request(&out.effects);
    assert_eq!(
        req["tools"],
        json!([]),
        "a prediction turn names no tools: {req}"
    );
    assert_eq!(
        req["all_tools_mode"], "",
        "and must not fall back to a mode's full tool set, which would let the \
         model call something: {req}"
    );
}

#[test]
fn a_completed_turn_records_the_candidate_and_returns_to_idle() {
    let (mut hsm, mut ctx) = seeded();
    hsm.dispatch(&Event::user_message("assess this change"), &mut ctx);

    hsm.dispatch(
        &Event::LlmTurnComplete {
            thread: "predict".into(),
            text: r#"{"risk":3}"#.into(),
            tool_calls: vec![],
        },
        &mut ctx,
    );

    assert_eq!(
        ctx.facts.get(RESULT_FACT).and_then(|v| v.as_str()),
        Some(r#"{"risk":3}"#),
        "the raw candidate must be recorded for the caller to validate"
    );
    assert_eq!(
        hsm.state(),
        PredictState::Idle,
        "and the machine must be ready for the next message - a repair attempt \
         arrives as an ordinary message, not as a special event"
    );
}

#[test]
fn a_failed_turn_returns_to_idle_and_records_the_error() {
    let (mut hsm, mut ctx) = seeded();
    hsm.dispatch(&Event::user_message("assess this change"), &mut ctx);

    hsm.dispatch(
        &Event::LlmFailed {
            error: "provider exploded".into(),
        },
        &mut ctx,
    );

    assert_eq!(
        hsm.state(),
        PredictState::Idle,
        "a provider failure must not strand the machine mid-turn"
    );
    assert!(
        ctx.facts
            .get("predict.error")
            .and_then(|v| v.as_str())
            .is_some_and(|e| e.contains("provider exploded")),
        "and the error must be recorded rather than silently swallowed"
    );
}

#[test]
fn a_second_message_starts_a_second_turn() {
    let (mut hsm, mut ctx) = seeded();
    hsm.dispatch(&Event::user_message("first"), &mut ctx);
    hsm.dispatch(
        &Event::LlmTurnComplete {
            thread: "predict".into(),
            text: "not json".into(),
            tool_calls: vec![],
        },
        &mut ctx,
    );

    // This is how a bounded repair attempt reaches the machine.
    let out = hsm.dispatch(&Event::user_message("that was not valid, retry"), &mut ctx);

    let req = turn_request(&out.effects);
    assert_eq!(
        req["instruction"], "that was not valid, retry",
        "a repair is an ordinary message carrying the diagnostic: {req}"
    );
    assert_eq!(
        req["schema"]["properties"]["risk"]["type"], "integer",
        "and still constrained by the same schema: {req}"
    );
}
