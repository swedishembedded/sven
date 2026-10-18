// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an exported trajectory says what the model actually saw and did.
//!
//! Training and evaluation tools read the trajectory as the record of a run,
//! so it must not present a compacted context as the whole conversation, list
//! tools the model was never offered, or turn a malformed tool call into an
//! empty one.
//!
//! Swedish Embedded AB implements agent trajectory capture for its clients.
//! If your team needs expertise in turning agent runs into training data then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

mod common;

use std::sync::{Arc, Mutex};

use common::Strict;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::config::Config;
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::{Agent, Engine};

/// Answers "ok" to everything, including the summarisation request a
/// compaction makes, and advertises a context window small enough that a few
/// long messages overflow it.
struct SmallWindow {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
impl ModelProvider for SmallWindow {
    fn name(&self) -> &str {
        "small-window"
    }
    fn model_name(&self) -> &str {
        "small-window"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(5000)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(100)
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.requests.lock().unwrap().push(req);
        let events = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn compaction_markers(agent: &Agent) -> Vec<serde_json::Value> {
    let json = serde_json::to_value(agent.trajectory()).unwrap();
    json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["extra"]["context_management"]["type"] == "compaction")
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_compaction_is_marked_in_the_trajectory_and_the_history_is_the_compacted_one() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut config = Config::default();
    config.agent.compaction_keep_recent = 2;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(SmallWindow {
            requests: Arc::clone(&requests),
        }))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");

    let long_message = "word ".repeat(450);
    let mut sent = 0;
    while compaction_markers(&agent).is_empty() {
        assert!(sent < 12, "no compaction after {sent} long messages");
        agent.send(&long_message).await.expect("an outcome");
        sent += 1;
    }

    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");
    let marker = &compaction_markers(&agent)[0];
    assert_eq!(marker["source"], "system");
    assert!(
        marker["extra"]["sven"]["tokens_before"].as_u64()
            > marker["extra"]["sven"]["tokens_after"].as_u64(),
        "{marker}"
    );

    // The history the next turn is seeded with is the compacted conversation
    // the kernel holds, not every message ever exchanged: it must be smaller
    // than the exchanges that produced it.
    let held = agent.state().history().len();
    assert!(
        held < sent * 2,
        "history kept {held} messages after {sent} exchanges"
    );

    // A resumed agent exports the same marker.
    let state: sven_sdk::AgentState =
        serde_json::from_str(&serde_json::to_string(agent.state()).unwrap()).unwrap();
    let resumed = engine.resume(state).expect("resumes");
    assert_eq!(compaction_markers(&resumed), compaction_markers(&agent));
}

/// A compaction shrinks what the model reads, not what happened: the exported
/// trajectory is the record of the run, so every exchange stays in it, in
/// order, with the compaction marked where it fired. A training record that
/// lost the steps a compaction replaced would teach from a conversation the
/// agent never had.
#[tokio::test]
async fn a_compaction_does_not_drop_the_steps_it_replaced_from_the_trajectory() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut config = Config::default();
    config.agent.compaction_keep_recent = 2;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(SmallWindow {
            requests: Arc::clone(&requests),
        }))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");

    let mut sent = 0;
    while compaction_markers(&agent).is_empty() || sent < 8 {
        assert!(sent < 14, "no compaction after {sent} long messages");
        agent
            .send(&format!("{} message {sent}", "word ".repeat(450)))
            .await
            .expect("an outcome");
        sent += 1;
    }

    let json = serde_json::to_value(agent.trajectory()).unwrap();
    let steps = json["steps"].as_array().unwrap();
    let user_steps: Vec<&serde_json::Value> =
        steps.iter().filter(|s| s["source"] == "user").collect();
    assert_eq!(user_steps.len(), sent, "every message sent is a user step");
    for (i, step) in user_steps.iter().enumerate() {
        assert!(
            step["message"]
                .as_str()
                .unwrap()
                .ends_with(&format!("message {i}")),
            "user step {i} is in order: {step}"
        );
    }
    assert_eq!(
        steps.iter().filter(|s| s["source"] == "agent").count(),
        sent,
        "and so is every answer"
    );
    // The marker stands where the compaction fired, not at the front.
    let first_marker = steps
        .iter()
        .position(|s| s["extra"]["context_management"]["type"] == "compaction")
        .expect("a marker");
    assert!(first_marker > 2, "marker at step {first_marker}");
    sven_sdk::atif::validate_trajectory(&agent.trajectory()).expect("a valid ATIF document");
}

struct Echo {
    name: &'static str,
    modes: &'static [sven_vocab::AgentMode],
}

#[async_trait::async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Echoes."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn modes(&self) -> &[sven_vocab::AgentMode] {
        self.modes
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "echo")
    }
}

fn scripted_engine(scripts: Vec<Vec<ResponseEvent>>, tools: Vec<Arc<dyn Tool>>) -> Engine {
    let mut builder =
        Engine::builder().model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))));
    for tool in tools {
        builder = builder.tool(tool);
    }
    builder.build().expect("an engine builds")
}

#[tokio::test]
async fn the_trajectory_lists_the_tools_offered_in_the_agents_mode() {
    use sven_vocab::AgentMode;
    let engine = scripted_engine(
        vec![vec![
            ResponseEvent::TextDelta("hi".into()),
            ResponseEvent::Done,
        ]],
        vec![
            Arc::new(Echo {
                name: "offered",
                modes: &[AgentMode::Agent],
            }),
            Arc::new(Echo {
                name: "research_only",
                modes: &[AgentMode::Research],
            }),
        ],
    );
    let mut agent = engine.agent("agent");
    agent.send("hello").await.expect("an outcome");

    let offered = serde_json::to_string(&agent.trajectory().agent.tool_definitions).unwrap();
    assert!(offered.contains("\"offered\""), "{offered}");
    assert!(!offered.contains("research_only"), "{offered}");
}

#[tokio::test]
async fn unparseable_tool_arguments_keep_their_raw_text_and_are_flagged() {
    let engine = scripted_engine(
        vec![
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "c1".into(),
                    name: "echo".into(),
                    arguments: "this is not json at all".into(),
                },
                ResponseEvent::Done,
            ],
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done],
        ],
        vec![Arc::new(Echo {
            name: "echo",
            modes: &[sven_vocab::AgentMode::Agent],
        })],
    );
    let mut agent = engine.agent("agent");
    agent.send("go").await.expect("an outcome");

    let json = serde_json::to_value(agent.trajectory()).unwrap();
    let call = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["tool_calls"].as_array().cloned().unwrap_or_default())
        .find(|c| c["tool_call_id"] == "c1")
        .expect("the call is recorded");
    assert_eq!(
        call["extra"]["raw_arguments"], "this is not json at all",
        "{call}"
    );
    assert_eq!(call["extra"]["unparsed_arguments"], true, "{call}");
}
