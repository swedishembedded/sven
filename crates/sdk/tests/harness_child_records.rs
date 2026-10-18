// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness run's trajectory holds its children's own records.
//!
//! A delegated child runs in a conversation of its own. When the parent's
//! trajectory is exported, that conversation is embedded as a trajectory of
//! its own - every message, the tool calls it made with their results, and the
//! account of each model request it made, sampling identity included - so a
//! consumer reading only the parent has everything that was done under it.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes for
//! its clients. If your team needs expertise in turning autonomous workflow
//! runs into reviewable training and audit records then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, RunOptions,
};

struct Notes;

#[async_trait::async_trait]
impl Tool for Notes {
    fn name(&self) -> &str {
        "read_notes"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Reads the notes."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "the notes say: blue")
    }
}

const PARENT: &str = r#"
api_version: "1"
name: parent
entry: down
capabilities: [SpawnChild, ReadFile]
limits: { max_steps: 20, max_delegate_depth: 2 }
steps:
  down:
    delegate: { target: worker, store_as: got, then: done }
  done:
    return: { value: "{{ steps.down.value }}" }
"#;

const WORKER: &str = r#"
api_version: "1"
name: worker
entry: work
capabilities: [ReadFile]
limits: { max_steps: 20, max_model_turns: 4, max_tool_calls: 4 }
steps:
  work:
    agent: { prompt: "Read the notes and report.", tools: [read_notes], store_as: said, then: done }
  done:
    return: { value: "{{ steps.work.value }}" }
"#;

fn usage() -> ResponseEvent {
    ResponseEvent::Usage {
        input_tokens: 30,
        output_tokens: 4,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

#[tokio::test]
async fn a_delegated_childs_conversation_and_sampling_identity_are_in_the_parents_trajectory() {
    let provider = ScriptedMockProvider::new(vec![
        vec![
            ResponseEvent::Served {
                model: "served-model".into(),
                seed: Some(5),
            },
            ResponseEvent::Tokens {
                ids: vec![11, 12],
                logprobs: vec![-0.5, -0.25],
            },
            ResponseEvent::PromptTokens { ids: vec![1, 2, 3] },
            ResponseEvent::ToolCall {
                index: 0,
                id: "n1".into(),
                name: "read_notes".into(),
                arguments: "{}".into(),
            },
            usage(),
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::Served {
                model: "served-model".into(),
                seed: Some(5),
            },
            ResponseEvent::Tokens {
                ids: vec![21],
                logprobs: vec![-0.125],
            },
            ResponseEvent::TextDelta("blue".into()),
            usage(),
            ResponseEvent::Done,
        ],
    ]);
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .tool(Arc::new(Notes))
        .build()
        .expect("an engine builds");
    engine.load_harness_from_str(WORKER).expect("worker loads");
    let parent = engine.load_harness_from_str(PARENT).expect("parent loads");
    let mut run = parent.start(json!({})).expect("starts");
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
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");
    let children = trajectory
        .subagent_trajectories
        .as_ref()
        .expect("the child is embedded");
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert!(child.trajectory_id.is_some(), "an embedded child has an id");

    // What the child did: it called the tool and got its result.
    let acted = child
        .steps
        .iter()
        .find(|s| {
            s.tool_calls
                .as_ref()
                .is_some_and(|c| c[0].function_name == "read_notes")
        })
        .expect("the child's tool call is in its record");
    let observed = serde_json::to_string(&acted.observation).unwrap();
    assert!(observed.contains("blue"), "{observed}");

    // And how it was sampled: the account of its model requests. The tool call
    // and the answer after its result are one step with two requests, so each
    // request's own prompt, tokens and seed are kept beside the run-together
    // step fields.
    let metrics = acted.metrics.as_ref().expect("the request's account");
    assert_eq!(metrics.completion_token_ids, Some(vec![11, 12, 21]));
    assert_eq!(metrics.prompt_token_ids, Some(vec![1, 2, 3]));
    assert_eq!(metrics.extra.as_ref().unwrap()["sven"]["seed"], 5);
    let requests = metrics.extra.as_ref().unwrap()["sven"]["requests"]
        .as_array()
        .expect("each request kept");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["completion_token_ids"], json!([11, 12]));
    assert_eq!(requests[0]["logprobs"], json!([-0.5, -0.25]));
    assert_eq!(requests[0]["prompt_token_ids"], json!([1, 2, 3]));
    assert_eq!(requests[1]["completion_token_ids"], json!([21]));
    assert_eq!(requests[1]["seed"], 5);
    assert_eq!(acted.model_name.as_deref(), Some("served-model"));

    // The run names the program it ran: its plan's digest (the digest of a
    // bundle with helpers is in `bundle_digest` when the harness was loaded as
    // one).
    assert_eq!(
        trajectory.extra.as_ref().unwrap()["harness"]["plan_digest"],
        json!(parent.digest())
    );
    assert_eq!(
        trajectory.extra.as_ref().unwrap()["harness"]["name"],
        "parent"
    );

    // The parent names its children.
    let listed = &trajectory.extra.as_ref().unwrap()["harness"]["children"];
    assert_eq!(listed[0]["trajectory_id"], json!(child.trajectory_id));
}
