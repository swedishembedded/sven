// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness document that declares `agent.profile: compact` gets the
//! compact run profile's tool definitions, and the trajectory it produces
//! names the profile's version.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in running one workflow on
//! models with very small context windows then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sven_model::{ResponseEvent, ToolSchema};
use sven_model_mock::CallbackProvider;
use sven_sdk::config::compact_profile;
use sven_sdk::{Engine, RunOptions, Toolset};

const WORKFLOW: &str = r#"
api_version: "2"
name: compact-run
agent:
  profile: compact
  system: "Work in the workspace."
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Do it.
    tools: [read_file, write_file, shell]
outputs:
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

#[tokio::test]
async fn a_compact_harness_offers_the_compact_definitions_and_records_the_profile() {
    let offered = Arc::new(Mutex::new(Vec::<Vec<ToolSchema>>::new()));
    let seen = Arc::clone(&offered);
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |req| {
            seen.lock().unwrap().push(req.tools.clone());
            vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
        })) as Arc<_>)
        // The profile, not the toolset, decides which built-in tools a
        // compact run has.
        .toolset(Toolset::none())
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );

    let tools = offered.lock().unwrap()[0].clone();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["read_file", "shell", "write_file"]);
    let read = tools.iter().find(|t| t.name == "read_file").unwrap();
    assert_eq!(read.parameters["properties"].as_object().unwrap().len(), 1);

    let trajectory = run.trajectory().expect("an executed run has one");
    assert_eq!(
        trajectory.agent.extra,
        Some(json!({"sven": {"profile": compact_profile::ID}}))
    );
}
