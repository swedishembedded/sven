// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a reply the provider cuts off keeps what it produced. The reasoning
//! of a turn cut inside an unclosed `<think>` block, and the name and raw
//! arguments of a tool call cut mid-argument, are recorded; and a cut-off call
//! is never run: the model is told, as a tool error, that it was truncated.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::CallbackProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, RunOptions,
};

struct WriteNote(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Tool for WriteNote {
    fn name(&self) -> &str {
        "write_note"
    }
    fn description(&self) -> &str {
        "Writes a note."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "written")
    }
}

async fn run(
    doc: &str,
    provider: CallbackProvider,
    tool_runs: Arc<AtomicUsize>,
) -> (sven_sdk::HarnessOutcome, Vec<(String, atif::Trajectory)>) {
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .tool(Arc::new(WriteNote(tool_runs)))
        .build()
        .expect("an engine builds");
    let mut run = engine
        .load_harness_from_str(doc)
        .expect("loads")
        .start(json!({}))
        .expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");
    (
        outcome,
        run.conversations().expect("an executed run has them"),
    )
}

#[tokio::test]
async fn a_turn_cut_inside_an_unclosed_think_block_keeps_its_reasoning() {
    const DOC: &str = r#"
api_version: "2"
name: cut
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    model_only: true
    task: Say a lot.
"#;
    let provider = CallbackProvider::new(|_| {
        vec![
            ResponseEvent::TextDelta("<think>weighing the options, still weighing".into()),
            ResponseEvent::MaxTokens,
            ResponseEvent::Done,
        ]
    });
    let (outcome, conversations) = run(DOC, provider, Arc::default()).await;
    assert!(
        format!("{:?}", outcome.result).contains("output_truncated"),
        "{:?}",
        outcome.result
    );
    let reasoning: Vec<_> = conversations
        .iter()
        .flat_map(|(_, c)| &c.steps)
        .filter_map(|s| s.reasoning_content.as_deref())
        .collect();
    assert_eq!(reasoning, ["weighing the options, still weighing"]);
}

#[tokio::test]
async fn a_tool_call_cut_mid_argument_is_not_run_and_the_model_is_told() {
    const DOC: &str = r#"
api_version: "2"
name: cut
capabilities: [ReadFile]
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Write the note.
    tools: [write_note]
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
"#;
    let results: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&results);
    let provider = CallbackProvider::new(move |req| {
        let told: Vec<String> = req
            .messages
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolResult { content, .. } => Some(format!("{content:?}")),
                _ => None,
            })
            .collect();
        if told.is_empty() {
            return vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "n1".into(),
                    name: "write_note".into(),
                    // Complete once closed: a repair would run it.
                    arguments: r#"{"text": "an unfinished"#.into(),
                },
                ResponseEvent::MaxTokens,
                ResponseEvent::Done,
            ];
        }
        seen.lock().unwrap().extend(told);
        vec![
            ResponseEvent::TextDelta("shorter next time".into()),
            ResponseEvent::Done,
        ]
    });
    let runs = Arc::new(AtomicUsize::new(0));
    let (outcome, conversations) = run(DOC, provider, Arc::clone(&runs)).await;
    assert_eq!(
        outcome.result.as_ref().unwrap().as_ref().unwrap()["stop"],
        "concluded"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a cut-off call must not run"
    );
    let told = results.lock().unwrap().join("\n");
    assert!(told.contains("truncat"), "the model is told why: {told}");

    let call = conversations
        .iter()
        .flat_map(|(_, c)| &c.steps)
        .flat_map(|s| s.tool_calls.iter().flatten())
        .next()
        .expect("the cut-off call is recorded");
    assert_eq!(call.function_name, "write_note");
    let extra = call.extra.as_ref().expect("the raw arguments are kept");
    assert_eq!(extra["raw_arguments"], r#"{"text": "an unfinished"#);
    assert_eq!(extra["unparsed_arguments"], true);
}
