// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a workflow package runs end to end on an engine. A `use` task starts
//! the imported workflow as a child with the inputs `with` names, reads the
//! outputs the child returns, fails when the child fails, and the child spends
//! under its parent's limits: the model turns of the whole tree never exceed
//! what the root declared.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in composing long-running agent
//! work from reviewed, reusable parts then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_harness_spec::HarnessBundle;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, RunOptions};

fn main_document(model_turns: u64) -> String {
    format!(
        r#"
api_version: "2"
name: caller
inputs: {{ repo: string }}
imports: {{ review: ./review.yaml }}
execution: {{ limits: {{ model_turns: {model_turns} }} }}
workflow:
  - name: prepare
    task: "Prepare {{{{ inputs.repo }}}}."
  - name: look
    use: review
    with: {{ target: "{{{{ inputs.repo }}}}" }}
outputs:
  verdict: {{ type: string, value: "{{{{ tasks.look.output.verdict }}}}" }}
"#
    )
}

const REVIEW: &str = r#"
api_version: "2"
name: reviewer
inputs: { target: string }
execution: { limits: { model_turns: 10 } }
workflow:
  - name: read
    task: "Read {{ inputs.target }}."
  - name: judge
    task: "Judge it."
outputs:
  verdict: { type: string, value: "{{ tasks.judge.output }}" }
"#;

/// A model that answers "fine" to its next twenty turns.
fn model() -> Arc<dyn sven_model::ModelProvider> {
    let turn = || vec![ResponseEvent::TextDelta("fine".into()), ResponseEvent::Done];
    Arc::new(ScriptedMockProvider::new((0..20).map(|_| turn()).collect()))
}

fn package(main: &str, review: &str) -> String {
    let bundle = HarnessBundle {
        document: main.to_string(),
        imports: BTreeMap::from([("review.yaml".to_string(), review.to_string())]),
    };
    serde_yaml::to_string(&bundle).expect("serializes")
}

async fn run(yaml: &str) -> sven_sdk::HarnessOutcome {
    let engine = Engine::builder()
        .model_provider(model())
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(yaml)
        .expect("the package loads");
    let mut run = harness.start(json!({ "repo": "r1" })).expect("starts");
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

#[tokio::test]
async fn a_use_task_runs_the_imported_workflow_and_hands_on_what_it_returns() {
    let outcome = run(&package(&main_document(20), REVIEW)).await;
    let Some(Ok(value)) = &outcome.result else {
        panic!("expected success: {:?}", outcome.result);
    };
    assert_eq!(value, &json!({ "verdict": "fine" }));
}

#[tokio::test]
async fn the_child_spends_under_its_parents_limits() {
    // The caller allows 3 model turns in all: one is its own, so the child,
    // which would take two of its own ten, has two left and fits...
    let fits = run(&package(&main_document(3), REVIEW)).await;
    assert!(matches!(&fits.result, Some(Ok(_))), "{:?}", fits.result);
    // ...but with only 2 in all, the child's second turn is one too many,
    // though the child's own limit would allow it.
    let outcome = run(&package(&main_document(2), REVIEW)).await;
    let Some(Err(failure)) = &outcome.result else {
        panic!("expected a failure: {:?}", outcome.result);
    };
    assert_eq!(failure.code, "child_failed", "{failure:?}");
    assert!(failure.message.contains("model-turn budget"), "{failure:?}");
}

#[tokio::test]
async fn a_failing_child_fails_the_caller_and_the_package_registers_once() {
    let failing = REVIEW.replace(
        "  - name: judge\n    task: \"Judge it.\"",
        "  - name: judge\n    task: \"Judge it.\"\n    verify: [{ artifact: never-written.txt }]",
    );
    let outcome = run(&package(&main_document(20), &failing)).await;
    let Some(Err(failure)) = &outcome.result else {
        panic!("expected a failure: {:?}", outcome.result);
    };
    assert_eq!(failure.code, "child_failed", "{failure:?}");

    // Loading the same package twice on one engine is refused, whole: the
    // imported name is taken.
    let engine = Engine::builder().model_provider(model()).build().unwrap();
    let yaml = package(&main_document(20), REVIEW);
    engine.load_harness_from_str(&yaml).expect("first load");
    assert!(engine.load_harness_from_str(&yaml).is_err());
}
