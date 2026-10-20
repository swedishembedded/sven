// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a task can name the model role that answers it; the host decides
//! which provider serves each role, a role the host does not offer fails the
//! turn instead of quietly using the default model, and no endpoint or key is
//! ever part of a document.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_model::ModelProvider;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, EngineBuilder, RunOptions};

const WORKFLOW: &str = r#"
api_version: "2"
name: roles
agent: { model: worker }
execution: { limits: { model_turns: 8 } }
workflow:
  - name: plan
    model_only: true
    model: planner
    task: Plan it.
  - name: work
    task: "Do {{ tasks.plan.output }}."
outputs:
  plan: { type: string, value: "{{ tasks.plan.output }}" }
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

fn provider(text: &str) -> Arc<dyn ModelProvider> {
    Arc::new(ScriptedMockProvider::always_text(text))
}

async fn run(builder: EngineBuilder) -> sven_sdk::HarnessOutcome {
    let engine: Engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

#[tokio::test]
async fn each_role_is_answered_by_the_provider_the_host_gave_it() {
    let outcome = run(Engine::builder()
        .model_provider(provider("from-default"))
        .model_role("planner", provider("from-planner"))
        .model_role("worker", provider("from-worker")))
    .await;
    let Some(Ok(value)) = &outcome.result else {
        panic!("{:?}", outcome.result)
    };
    assert_eq!(value["plan"], "from-planner");
    assert_eq!(value["done"], "from-worker");
}

#[tokio::test]
async fn a_role_the_host_does_not_offer_fails_the_run_and_does_not_fall_back() {
    let outcome = run(Engine::builder()
        .model_provider(provider("from-default"))
        .model_role("worker", provider("from-worker")))
    .await;
    let Some(Err(failure)) = &outcome.result else {
        panic!("{:?}", outcome.result)
    };
    assert!(failure.message.contains("planner"), "{failure:?}");
}

#[tokio::test]
async fn without_any_role_the_default_provider_is_not_used_for_a_named_role() {
    let outcome = run(Engine::builder().model_provider(provider("from-default"))).await;
    assert!(
        matches!(&outcome.result, Some(Err(_))),
        "{:?}",
        outcome.result
    );
}
