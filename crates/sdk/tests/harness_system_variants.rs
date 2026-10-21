// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: `agent.system` may be a list of `{when, text}` entries; the first
//! whose condition (over inputs) holds is the system message of the agent's
//! conversation, so one document serves runs that need different standing
//! instructions. The chosen text is what the model is sent and what the
//! trajectory's system step records.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in running one workflow under
//! several standing instructions then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, RunOptions};

const WORKFLOW: &str = r#"
api_version: "2"
name: variants
inputs: { mode: string, subject: string }
agent:
  system:
    - when: { eq: inputs.mode, value: strict }
      text: "Be strict about {{ inputs.subject }}."
    - when: { eq: inputs.mode, value: lenient }
      text: "Be lenient about {{ inputs.subject }}."
    - text: "Be neutral about {{ inputs.subject }}."
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Do it.
outputs:
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

/// Runs the workflow with `mode`; returns the system message the model was
/// sent and the one the trajectory recorded.
async fn system_for(mode: &str) -> (String, String) {
    let sent = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&sent);
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |req| {
            let system = req
                .messages
                .iter()
                .find(|m| m.role == sven_model::Role::System)
                .and_then(|m| m.as_text())
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push(system);
            vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
        })) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let mut run = harness
        .start(json!({ "mode": mode, "subject": "tests" }))
        .expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );

    let trajectory = run.trajectory().expect("an executed run has one");
    let recorded = trajectory
        .steps
        .iter()
        .find(|s| format!("{:?}", s.source) == "System")
        .map(|s| serde_json::to_value(&s.message).unwrap())
        .expect("a system step");
    let sent = sent.lock().unwrap()[0].clone();
    (sent, recorded.as_str().unwrap_or_default().to_string())
}

#[tokio::test]
async fn the_first_variant_whose_condition_holds_is_the_system_message() {
    for (mode, text) in [
        ("strict", "Be strict about tests."),
        ("lenient", "Be lenient about tests."),
        ("other", "Be neutral about tests."),
    ] {
        let (sent, recorded) = system_for(mode).await;
        assert_eq!(sent, text, "{mode}: what the model was sent");
        assert_eq!(recorded, text, "{mode}: the trajectory's system step");
    }
}
