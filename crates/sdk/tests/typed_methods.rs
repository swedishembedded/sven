// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a model-driven method is an ordinary typed call.
//!
//! The caller supplies typed input and receives a validated result or an
//! explicit failure. The model calls, the schema, and any correction attempts
//! live behind that boundary.
//!
//! Swedish Embedded AB implements typed agent interfaces for its clients. If
//! your team needs expertise in constraining model output to real application
//! types then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sven_model::{ResponseEvent, ResponseFormat};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{CallError, Engine, Method};

#[derive(Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
struct Assessment {
    /// How risky the change is, 0-100.
    risk: u8,
    /// One sentence of justification.
    summary: String,
}

#[derive(Debug, Serialize)]
struct Change {
    diff: String,
}

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

fn assess() -> Method<Assessment> {
    Method::new("assess")
        .role("You are a meticulous Rust code reviewer.")
        .task("Assess the change for correctness risk.")
}

fn engine_with(scripts: Vec<Vec<ResponseEvent>>) -> (Engine, Arc<ScriptedMockProvider>) {
    let provider = Arc::new(ScriptedMockProvider::new(scripts));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds");
    (engine, provider)
}

fn a_change() -> Change {
    Change {
        diff: "- let x = 1;\n+ let x = 2;".into(),
    }
}

#[tokio::test]
async fn a_method_returns_a_validated_value_of_its_return_type() {
    let (engine, _p) = engine_with(vec![says(r#"{"risk":3,"summary":"looks fine"}"#)]);
    let mut agent = engine.agent_for(&assess());

    let got = agent.call(&assess(), &a_change()).await.expect("a result");

    assert_eq!(
        got,
        Assessment {
            risk: 3,
            summary: "looks fine".into(),
        }
    );
}

#[tokio::test]
async fn the_request_carries_a_schema_derived_from_the_return_type() {
    let (engine, provider) = engine_with(vec![says(r#"{"risk":1,"summary":"ok"}"#)]);
    let mut agent = engine.agent_for(&assess());

    agent.call(&assess(), &a_change()).await.expect("a result");

    let seen = provider
        .last_request
        .lock()
        .unwrap()
        .clone()
        .expect("the provider saw a request");
    match seen.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            assert_eq!(name, "assess", "the schema is named after the method");
            assert!(
                schema["properties"]["risk"].is_object(),
                "and derived from the return type rather than hand-written: {schema}"
            );
        }
        other => panic!("expected a JSON-schema constraint, got {other:?}"),
    }
}

#[tokio::test]
async fn the_model_is_offered_no_tools_for_a_prediction() {
    let (engine, provider) = engine_with(vec![says(r#"{"risk":1,"summary":"ok"}"#)]);
    let mut agent = engine.agent_for(&assess());

    agent.call(&assess(), &a_change()).await.expect("a result");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    assert!(
        seen.tools.is_empty(),
        "a prediction must not be able to call anything: {:?}",
        seen.tools
    );
}

#[tokio::test]
async fn a_malformed_answer_is_repaired_within_the_budget() {
    let (engine, _p) = engine_with(vec![
        says("I think it's probably fine, honestly"),
        says(r#"{"risk":7,"summary":"repaired"}"#),
    ]);
    let mut agent = engine.agent_for(&assess());

    let got = agent
        .call(&assess().max_repairs(2), &a_change())
        .await
        .expect("a result after one repair");

    assert_eq!(got.risk, 7);
}

#[tokio::test]
async fn an_exhausted_budget_is_a_model_failure_carrying_a_diagnostic() {
    let (engine, _p) = engine_with(vec![says("nope"), says("still nope"), says("nope again")]);
    let mut agent = engine.agent_for(&assess());

    let err = agent
        .call(&assess().max_repairs(1), &a_change())
        .await
        .expect_err("two bad answers exhaust a one-repair budget");

    match err {
        CallError::Invalid {
            attempts, detail, ..
        } => {
            assert_eq!(attempts, 2, "the first attempt plus one repair");
            assert!(
                !detail.is_empty(),
                "the failure must say what was wrong with the answer"
            );
        }
        other => panic!("a bad answer is a model failure, not {other:?}"),
    }
}

#[tokio::test]
async fn a_postcondition_failure_is_distinct_from_a_malformed_answer() {
    // Structurally perfect, but violates an invariant the type cannot express.
    let (engine, _p) = engine_with(vec![says(r#"{"risk":200,"summary":"over"}"#)]);
    let mut agent = engine.agent_for(&assess());

    let method = assess().max_repairs(0).postcondition(|a: &Assessment| {
        if a.risk <= 100 {
            Ok(())
        } else {
            Err(format!("risk {} is outside 0-100", a.risk))
        }
    });

    let err = agent
        .call(&method, &a_change())
        .await
        .expect_err("the invariant rejects it");

    assert!(
        matches!(err, CallError::Postcondition { .. }),
        "a well-formed answer that breaks an invariant is not a parse failure: {err:?}"
    );
}

#[tokio::test]
async fn a_postcondition_failure_can_be_repaired_too() {
    let (engine, _p) = engine_with(vec![
        says(r#"{"risk":200,"summary":"over"}"#),
        says(r#"{"risk":40,"summary":"within range"}"#),
    ]);
    let mut agent = engine.agent_for(&assess());

    let method = assess().max_repairs(1).postcondition(|a: &Assessment| {
        if a.risk <= 100 {
            Ok(())
        } else {
            Err(format!("risk {} is outside 0-100", a.risk))
        }
    });

    let got = agent.call(&method, &a_change()).await.expect("a result");
    assert_eq!(got.risk, 40, "the invariant's message is usable feedback");
}

#[tokio::test]
async fn the_same_contract_is_served_by_either_strategy() {
    // Identical method, identical call site, different execution strategy.
    // Only the machine behind it changes - which is the point: strategy is
    // configuration, not something the caller has to thread through.
    for strategy in [sven_sdk::Strategy::Predict, sven_sdk::Strategy::Investigate] {
        let (engine, _p) = engine_with(vec![says(r#"{"risk":5,"summary":"same"}"#)]);
        let method = assess().strategy(strategy);
        let mut agent = engine.agent_for(&method);

        let got = agent
            .call(&method, &a_change())
            .await
            .unwrap_or_else(|e| panic!("{strategy:?} failed: {e}"));

        assert_eq!(got.risk, 5, "{strategy:?} produced a different result");
    }
}

#[tokio::test]
async fn an_investigating_method_may_use_tools() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![says(
        r#"{"risk":5,"summary":"checked"}"#,
    )]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .toolset(sven_sdk::Toolset::research())
        .build()
        .expect("an engine builds");
    let method = assess().strategy(sven_sdk::Strategy::Investigate);
    let mut agent = engine.agent_for(&method);

    agent.call(&method, &a_change()).await.expect("a result");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    assert!(
        !seen.tools.is_empty(),
        "investigation is the strategy that gets tools; without them it is \
         just a prediction with extra steps"
    );
}

#[tokio::test]
async fn a_repair_attempt_sees_the_answer_it_is_correcting() {
    let (engine, provider) = engine_with(vec![
        says("I reckon it's fine"),
        says(r#"{"risk":7,"summary":"repaired"}"#),
    ]);
    let mut agent = engine.agent_for(&assess());

    agent
        .call(&assess().max_repairs(1), &a_change())
        .await
        .expect("a repaired result");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    let rendered = format!("{:?}", seen.messages);
    assert!(
        rendered.contains("I reckon it's fine"),
        "the rejected answer must still be in the thread on the repair turn - \
         a correction the model cannot see the mistake for is just the same \
         question asked twice: {rendered}"
    );
    assert!(
        rendered.contains("rejected"),
        "and the diagnostic must follow it: {rendered}"
    );
}

#[tokio::test]
async fn a_cancelled_call_stops_instead_of_answering() {
    let (engine, _p) = engine_with(vec![says(r#"{"risk":3,"summary":"looks fine"}"#)]);
    let mut agent = engine.agent_for(&assess());
    let cancel = sven_sdk::CancelToken::new();
    cancel.cancel();

    let got = agent
        .call_with(
            &assess(),
            &a_change(),
            sven_sdk::RunOptions::new().cancel(cancel),
        )
        .await;

    assert!(
        matches!(
            got,
            Err(CallError::Stopped {
                conclusion: sven_sdk::RunConclusion::Cancelled
            })
        ),
        "{got:?}"
    );
}

#[tokio::test]
async fn the_token_budget_spans_every_repair_attempt() {
    let spend = |text: &str| {
        vec![
            ResponseEvent::TextDelta(text.to_string()),
            ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 30,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            ResponseEvent::Done,
        ]
    };
    // The first answer is not an Assessment; a repair would cost another
    // 30 tokens, which a 40-token budget for the whole call cannot pay.
    let (engine, _p) = engine_with(vec![
        spend("not json"),
        spend(r#"{"risk":3,"summary":"fine"}"#),
    ]);
    let mut agent = engine.agent_for(&assess());

    let got = agent
        .call_with(
            &assess(),
            &a_change(),
            sven_sdk::RunOptions::new().max_output_tokens(40),
        )
        .await;

    assert!(
        matches!(
            got,
            Err(CallError::Stopped {
                conclusion: sven_sdk::RunConclusion::BudgetExhausted
            })
        ),
        "{got:?}"
    );
}
