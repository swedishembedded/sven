// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness run holds three independent caps - the capabilities its
//! document declares, the tool allowlist of the agent step, and the ceiling
//! the host puts on the engine - and an agent holds only what all three
//! allow. No proposal of the model widens any of them.
//!
//! Swedish Embedded AB implements authorization for programmable agent
//! runtimes for its clients. If your team needs expertise in bounding what
//! autonomous workflows may do then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, EngineBuilder, RunOptions,
};

/// A tool that counts its executions, under the name and capability it is
/// built with.
struct Counting {
    name: &'static str,
    capability: ToolCapability,
    runs: Arc<AtomicUsize>,
}

impl Counting {
    fn new(name: &'static str, capability: ToolCapability) -> (Arc<Self>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let tool = Arc::new(Self {
            name,
            capability,
            runs: Arc::clone(&runs),
        });
        (tool, runs)
    }
}

#[async_trait::async_trait]
impl Tool for Counting {
    fn name(&self) -> &str {
        self.name
    }
    fn kernel_capability(&self) -> ToolCapability {
        self.capability
    }
    fn description(&self) -> &str {
        "counts its runs"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.runs.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "done")
    }
}

fn proposes(names: &[&str]) -> Vec<ResponseEvent> {
    let mut events: Vec<ResponseEvent> = names
        .iter()
        .enumerate()
        .map(|(index, name)| ResponseEvent::ToolCall {
            index: u32::try_from(index).unwrap(),
            id: format!("call-{index}"),
            name: (*name).to_string(),
            arguments: "{}".to_string(),
        })
        .collect();
    events.push(ResponseEvent::Done);
    events
}

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done]
}

async fn run(harness: &sven_sdk::Harness) -> sven_sdk::HarnessOutcome {
    let mut run = harness.start(json!({})).expect("inputs check out");
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run.execute(RunOptions::new()),
    )
    .await
    .expect("the run ends")
    .expect("an outcome")
}

/// The tools of the three-cap scenarios: a read, a write and a shell.
struct Tools {
    read: Arc<AtomicUsize>,
    write: Arc<AtomicUsize>,
    shell: Arc<AtomicUsize>,
}

fn with_tools(builder: EngineBuilder) -> (EngineBuilder, Tools) {
    let (read, read_runs) = Counting::new("read_notes", ToolCapability::ReadFile);
    let (write, write_runs) = Counting::new("write_notes", ToolCapability::WriteFile);
    let (shell, shell_runs) = Counting::new("run_shell", ToolCapability::ExecuteShell);
    (
        builder.tool(read).tool(write).tool(shell),
        Tools {
            read: read_runs,
            write: write_runs,
            shell: shell_runs,
        },
    )
}

fn count(runs: &Arc<AtomicUsize>) -> usize {
    runs.load(Ordering::SeqCst)
}

/// The agent is open (no allowlist): the document's capabilities alone
/// decide, then the host's ceiling.
const OPEN_AGENT: &str = r#"
api_version: "1"
name: open-agent
entry: work
capabilities: [ReadFile, WriteFile]
limits: { max_steps: 20, max_model_turns: 4, max_tool_calls: 8 }
steps:
  work:
    agent: { prompt: "Work.", then: done }
  done:
    return: { value: "finished" }
"#;

fn model_proposing_all() -> Arc<ScriptedMockProvider> {
    Arc::new(ScriptedMockProvider::new(vec![
        proposes(&["read_notes", "write_notes", "run_shell"]),
        says("done"),
    ]))
}

#[tokio::test]
async fn agent_cannot_expand_capabilities_beyond_the_document() {
    let (builder, tools) = with_tools(Engine::builder().model_provider(model_proposing_all()));
    let engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(OPEN_AGENT).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(count(&tools.read), 1, "declared and offered");
    assert_eq!(count(&tools.write), 1, "declared and offered");
    assert_eq!(count(&tools.shell), 0, "ExecuteShell was never declared");
}

#[tokio::test]
async fn agent_cannot_expand_capabilities_beyond_the_host_ceiling() {
    let (builder, tools) = with_tools(
        Engine::builder()
            .model_provider(model_proposing_all())
            .host_capabilities([ToolCapability::ReadFile]),
    );
    let engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(OPEN_AGENT).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(count(&tools.read), 1, "inside both caps");
    assert_eq!(
        count(&tools.write),
        0,
        "the document declares WriteFile but the host does not allow it"
    );
    assert_eq!(count(&tools.shell), 0);
}

#[tokio::test]
async fn host_ceiling_cannot_grant_what_the_document_does_not_declare() {
    let (builder, tools) = with_tools(
        Engine::builder()
            .model_provider(model_proposing_all())
            .host_capabilities(ToolCapability::ALL),
    );
    let engine = builder.build().expect("an engine builder");
    let harness = engine.load_harness_from_str(OPEN_AGENT).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(count(&tools.shell), 0, "a host ceiling is never a grant");
}

#[tokio::test]
async fn host_ceiling_stops_a_tool_step_before_it_runs() {
    const WRITES: &str = r#"
api_version: "1"
name: writes
entry: save
capabilities: [WriteFile]
limits: { max_steps: 20 }
steps:
  save: { tool: { name: write_notes, store_as: saved, then: done } }
  done: { return: { value: "saved" } }
"#;
    let (builder, tools) = with_tools(
        Engine::builder()
            .model_provider(model_proposing_all())
            .host_capabilities([ToolCapability::ReadFile]),
    );
    let engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(WRITES).expect("loads");

    let outcome = run(&harness).await;

    assert_eq!(count(&tools.write), 0);
    assert!(
        !matches!(&outcome.result, Some(Ok(_))),
        "a refused step cannot succeed: {:?}",
        outcome.result
    );
}

const CHILD: &str = r#"
api_version: "1"
name: writer
entry: save
capabilities: [WriteFile]
limits: { max_steps: 20 }
steps:
  save: { tool: { name: write_notes, then: done } }
  done: { return: { value: "saved" } }
"#;

const PARENT: &str = r#"
api_version: "1"
name: parent
entry: go
capabilities: [SpawnChild, WriteFile]
limits: { max_steps: 20, max_delegate_depth: 3 }
steps:
  go: { delegate: { target: writer, then: done } }
  done: { return: { value: "ok" } }
"#;

/// How often the child's write ran under an engine with this ceiling.
async fn child_writes(ceiling: Option<Vec<ToolCapability>>) -> usize {
    let mut builder = Engine::builder().model_provider(model_proposing_all());
    if let Some(ceiling) = ceiling {
        builder = builder.host_capabilities(ceiling);
    }
    let (builder, tools) = with_tools(builder);
    let engine = builder.build().expect("an engine builds");
    engine.load_harness_from_str(CHILD).expect("child loads");
    let parent = engine.load_harness_from_str(PARENT).expect("parent loads");
    let _ = run(&parent).await;
    count(&tools.write)
}

#[tokio::test]
async fn host_ceiling_holds_for_a_delegated_child() {
    assert_eq!(child_writes(None).await, 1, "control: the child writes");
    assert_eq!(
        child_writes(Some(vec![
            ToolCapability::ReadFile,
            ToolCapability::SpawnChild
        ]))
        .await,
        0,
        "the child inherits the ceiling"
    );
}

#[tokio::test]
async fn an_engine_without_a_ceiling_keeps_the_documents_own_authority() {
    let (builder, tools) = with_tools(Engine::builder().model_provider(model_proposing_all()));
    let engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(OPEN_AGENT).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(
        count(&tools.write),
        1,
        "no ceiling configured, no narrowing"
    );
}
