// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the sampling and thinking settings a workflow sets per task reach the
//! model request of that task, and the trajectory records, for each request,
//! the settings it was sent with.
//!
//! Swedish Embedded AB implements reproducible agent runs on locally served
//! models for its clients. If your team needs expertise in recording exactly
//! how an agent's model was sampled then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::{Engine, RunOptions};

const WORKFLOW: &str = r#"
api_version: "2"
name: decoded
inputs: { role: string }
agent:
  system: "You work as {{ inputs.role }}."
  sampling: { temperature: 0.7 }
  thinking: { enabled: true }
execution: { limits: { model_turns: 8 } }
workflow:
  - name: plan
    model_only: true
    system: "You plan for {{ inputs.role }}."
    sampling: { temperature: 0.1, seed: 3 }
    thinking: { enabled: false, budget: 128 }
    task: Plan it.
  - name: work
    task: Do it.
outputs:
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

/// What a request was sent with: its temperature and its thinking switch.
type Sent = (Option<f64>, Option<bool>);

/// Answers "ok" and keeps what each request was sent with.
#[derive(Default)]
struct Capturing {
    seen: Arc<Mutex<Vec<Sent>>>,
    conversations: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ModelProvider for Capturing {
    fn name(&self) -> &str {
        "capturing"
    }
    fn model_name(&self) -> &str {
        "m"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.conversations.lock().unwrap().push(
            req.messages
                .iter()
                .map(|m| format!("{:?}: {}", m.role, m.as_text().unwrap_or_default()))
                .collect(),
        );
        self.seen
            .lock()
            .unwrap()
            .push((req.sampling.temperature, req.enable_thinking));
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn each_tasks_settings_reach_its_request_and_the_trajectory_records_them() {
    let provider = Capturing::default();
    let seen = Arc::clone(&provider.seen);
    let conversations = Arc::clone(&provider.conversations);
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<dyn ModelProvider>)
        .project_root(dir.path())
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let mut run = harness
        .start(json!({ "role": "a tester" }))
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

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(Some(0.1), Some(false)), (Some(0.7), Some(true))],
        "the planner and the worker are sampled differently"
    );

    // Each role has its own conversation, opened by its own system message.
    assert_eq!(
        *conversations.lock().unwrap(),
        vec![
            vec![
                "System: You plan for a tester.".to_string(),
                "User: Plan it.".to_string()
            ],
            vec![
                "System: You work as a tester.".to_string(),
                "User: Do it.".to_string()
            ],
        ]
    );

    let trajectory = run.trajectory().expect("an executed run has one");
    let decodings = |t: &atif::Trajectory| -> Vec<serde_json::Value> {
        t.steps
            .iter()
            .filter_map(|s| {
                s.metrics.as_ref()?.extra.as_ref()?["sven"]
                    .get("decoding")
                    .cloned()
            })
            .collect()
    };
    let sources = |t: &atif::Trajectory| -> Vec<String> {
        t.steps.iter().map(|s| format!("{:?}", s.source)).collect()
    };
    // The worker's conversation: its system message, its task, its answer.
    assert_eq!(sources(&trajectory), ["System", "User", "Agent"]);
    assert_eq!(
        decodings(&trajectory),
        vec![json!({ "temperature": 0.7, "thinking": true })]
    );
    // Each user turn says where it came from.
    let origins = |t: &atif::Trajectory| -> Vec<Option<String>> {
        t.steps
            .iter()
            .filter(|s| format!("{:?}", s.source) == "User")
            .map(|s| {
                s.extra.as_ref()?["sven"]["origin"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    };
    assert_eq!(origins(&trajectory), [Some("task".to_string())]);
    // The planner's conversation is its own trajectory, named, embedded.
    let embedded = trajectory
        .subagent_trajectories
        .as_ref()
        .expect("the planner's conversation");
    assert_eq!(embedded.len(), 1);
    assert_eq!(
        embedded[0].extra.as_ref().unwrap()["harness"]["conversation"],
        "plan"
    );
    assert_eq!(sources(&embedded[0]), ["System", "User", "Agent"]);
    assert_eq!(
        decodings(&embedded[0]),
        vec![json!({ "temperature": 0.1, "seed": 3, "thinking": false, "thinking_budget": 128 })]
    );
    assert_eq!(
        trajectory.extra.as_ref().unwrap()["harness"]["conversations"],
        json!(["plan"])
    );
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");
}
