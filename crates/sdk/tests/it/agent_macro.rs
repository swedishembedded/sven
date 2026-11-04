// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent is declared as a Rust trait, and its documentation is its
//! prompt.
//!
//! The point of the macro is that there is nowhere for a prompt to drift to.
//! The role is the trait's doc comment, each task is its method's doc comment,
//! and each schema is derived from the method's return type - so changing the
//! contract and changing what the model is told are the same edit.
//!
//! Swedish Embedded AB implements typed agent interfaces for its clients. If
//! your team needs expertise in keeping model instructions and application
//! types from drifting apart then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sven_model::{ResponseEvent, ResponseFormat};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{agent, Engine};

#[derive(Debug, PartialEq, Deserialize, JsonSchema)]
struct Assessment {
    risk: u8,
    summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Change {
    diff: String,
}

/// You are a meticulous Rust reviewer who never speculates.
#[agent]
trait Reviewer {
    /// Assess the change for correctness risk.
    async fn assess(&self, change: Change) -> Assessment;

    /// Decide whether a change may merge. Deterministic: no model involved.
    fn may_merge(&self, assessment: &Assessment) -> bool {
        assessment.risk < 50
    }
}

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
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
async fn a_declared_method_is_called_like_any_other_method() {
    let (engine, _p) = engine_with(vec![says(r#"{"risk":12,"summary":"minor"}"#)]);
    let mut reviewer = Reviewer::new(&engine);

    let assessment = reviewer.assess(a_change()).await.expect("an assessment");

    assert_eq!(
        assessment,
        Assessment {
            risk: 12,
            summary: "minor".into(),
        }
    );
}

#[tokio::test]
async fn the_trait_doc_becomes_the_role_and_the_method_doc_the_task() {
    let (engine, provider) = engine_with(vec![says(r#"{"risk":1,"summary":"ok"}"#)]);
    let mut reviewer = Reviewer::new(&engine);

    reviewer.assess(a_change()).await.expect("an assessment");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    let rendered = format!("{:?}", seen.messages);
    assert!(
        rendered.contains("meticulous Rust reviewer"),
        "the trait's documentation must reach the model as its role: {rendered}"
    );
    assert!(
        rendered.contains("Assess the change for correctness risk"),
        "and the method's documentation as the task: {rendered}"
    );
}

#[tokio::test]
async fn the_schema_comes_from_the_declared_return_type() {
    let (engine, provider) = engine_with(vec![says(r#"{"risk":1,"summary":"ok"}"#)]);
    let mut reviewer = Reviewer::new(&engine);

    reviewer.assess(a_change()).await.expect("an assessment");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    match seen.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            assert_eq!(name, "assess", "named after the method");
            assert!(
                schema["properties"]["summary"].is_object(),
                "and derived from the return type: {schema}"
            );
        }
        other => panic!("expected a schema constraint, got {other:?}"),
    }
}

#[tokio::test]
async fn a_method_with_a_body_stays_ordinary_code() {
    let (engine, provider) = engine_with(vec![says(r#"{"risk":90,"summary":"risky"}"#)]);
    let mut reviewer = Reviewer::new(&engine);

    let assessment = reviewer.assess(a_change()).await.expect("an assessment");
    let calls_before = provider.last_request.lock().unwrap().clone();

    assert!(
        !reviewer.may_merge(&assessment),
        "the policy check must apply its own rule"
    );
    let calls_after = provider.last_request.lock().unwrap().clone();

    assert_eq!(
        format!("{calls_before:?}"),
        format!("{calls_after:?}"),
        "a method with a body is deterministic - it must not reach the model"
    );
}

#[tokio::test]
async fn a_declared_agent_suspends_and_resumes_like_any_other() {
    let (engine, _p) = engine_with(vec![
        says(r#"{"risk":1,"summary":"first"}"#),
        says(r#"{"risk":2,"summary":"second"}"#),
    ]);

    let mut reviewer = Reviewer::new(&engine);
    reviewer.assess(a_change()).await.expect("an assessment");

    let stored = serde_json::to_string(&reviewer.suspend()).expect("serializable");
    let mut resumed = Reviewer::resume(
        &engine,
        serde_json::from_str(&stored).expect("deserializable"),
    )
    .expect("resumable");

    let second = resumed.assess(a_change()).await.expect("an assessment");
    assert_eq!(second.summary, "second");
}
