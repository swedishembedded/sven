// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness run is authorized end to end - the host's approval mode,
//! the document's own declaration (for a delegated child, its own and not
//! its parent's) and the tool registry's real capability all bound every
//! call, whichever way the call was reached.
//!
//! Swedish Embedded AB implements authorization for programmable agent
//! runtimes for its clients. If your team needs expertise in bounding what
//! autonomous workflows may do then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sven_harness_spec::HarnessBundle;
use sven_model::ResponseEvent;
use sven_model_mock::{FailingMockProvider, ScriptedMockProvider};
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    ApprovalPolicy, Engine, EngineBuilder, HumanGate, RunConclusion, RunOptions,
};

/// A tool that counts its executions, under whatever name and capability it
/// is built with.
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

fn builder() -> EngineBuilder {
    Engine::builder().model_provider(Arc::new(FailingMockProvider::new("no model here")) as Arc<_>)
}

/// A model that has the agent call `tool` once and then say it is done.
fn calls(tool: &str) -> Arc<dyn sven_model::ModelProvider> {
    Arc::new(ScriptedMockProvider::tool_then_text(
        "c1", tool, "{}", "done",
    ))
}

/// A package: a main workflow and the workflows it imports, by path.
fn package(main: &str, imports: &[(&str, &str)]) -> String {
    let bundle = HarnessBundle {
        document: main.to_string(),
        imports: imports
            .iter()
            .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
            .collect::<BTreeMap<_, _>>(),
    };
    serde_yaml::to_string(&bundle).expect("a package serializes")
}

/// The approvals a host handler saw, as `capability tool` words.
type Asked = Arc<Mutex<Vec<String>>>;

/// A builder whose host answers every approval with `answer`, recording the
/// non-read gates it was asked about.
fn manual_builder(
    answer: bool,
    model: Arc<dyn sven_model::ModelProvider>,
) -> (EngineBuilder, Asked) {
    let asked: Asked = Arc::default();
    let log = Arc::clone(&asked);
    let builder = Engine::builder()
        .model_provider(model)
        .approvals(ApprovalPolicy::Manual)
        .human_gates(move |gate| match gate {
            HumanGate::Approval {
                capability,
                call,
                reply_tx,
                ..
            } => {
                let name = call.map(|c| c.name).unwrap_or_default();
                log.lock().unwrap().push(format!("{capability:?} {name}"));
                let _ = reply_tx.send(answer);
            }
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(String::new());
            }
        });
    (builder, asked)
}

/// Runs `harness` to its end on a bounded clock.
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

const WRITES: &str = r#"
api_version: "2"
name: writes
capabilities: [WriteFile]
agent: { tools: [write_notes] }
execution: { limits: { model_turns: 4, tool_calls: 2 } }
workflow:
  - name: save
    task: Save the notes.
"#;

#[tokio::test]
async fn manual_approval_is_asked_before_a_harness_tool_call_runs() {
    let (tool, runs) = Counting::new("write_notes", ToolCapability::WriteFile);
    let (builder, asked) = manual_builder(false, calls("write_notes"));
    let engine = builder.tool(tool).build().expect("an engine builds");
    let harness = engine.load_harness_from_str(WRITES).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(
        asked.lock().unwrap().as_slice(),
        ["WriteFile write_notes"],
        "the host was asked about the write"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "refused, so it never ran");
}

const PARENT: &str = r#"
api_version: "2"
name: parent
capabilities: [SpawnChild, WriteFile]
imports: { child: ./child.yaml }
execution: { limits: { model_turns: 6, delegate_depth: 3 } }
workflow:
  - name: hand_over
    use: child
"#;

const CHILD_DECLARING_NOTHING: &str = r#"
api_version: "2"
name: child
capabilities: []
agent: { tools: [write_notes] }
execution: { limits: { model_turns: 4, tool_calls: 2 } }
workflow:
  - name: write
    task: Write the notes.
"#;

const CHILD_DECLARING_WRITE: &str = r#"
api_version: "2"
name: child
capabilities: [WriteFile]
agent: { tools: [write_notes] }
execution: { limits: { model_turns: 4, tool_calls: 2 } }
workflow:
  - name: write
    task: Write the notes.
"#;

#[tokio::test]
async fn a_delegated_child_holds_no_capability_its_own_document_does_not_declare() {
    let (tool, runs) = Counting::new("write_notes", ToolCapability::WriteFile);
    let model = Arc::new(ScriptedMockProvider::tool_then_text(
        "c1",
        "write_notes",
        "{}",
        "done",
    ));
    let last_request = Arc::clone(&model.last_request);
    let engine = Engine::builder()
        .model_provider(model)
        .tool(tool)
        .build()
        .expect("an engine builds");
    let parent = engine
        .load_harness_from_str(&package(PARENT, &[("child.yaml", CHILD_DECLARING_NOTHING)]))
        .expect("the package loads");

    let _ = run(&parent).await;

    assert_eq!(runs.load(Ordering::SeqCst), 0, "the child may not write");
    let told = format!("{:?}", last_request.lock().unwrap());
    assert!(
        told.contains("permission denied"),
        "the child's agent is told why the call was refused, got {told}"
    );
}

#[tokio::test]
async fn a_delegated_childs_calls_are_still_asked_under_manual_approval() {
    let (tool, runs) = Counting::new("write_notes", ToolCapability::WriteFile);
    let (builder, asked) = manual_builder(false, calls("write_notes"));
    let engine = builder.tool(tool).build().expect("an engine builds");
    let parent = engine
        .load_harness_from_str(&package(PARENT, &[("child.yaml", CHILD_DECLARING_WRITE)]))
        .expect("the package loads");

    let _ = run(&parent).await;

    assert_eq!(
        asked.lock().unwrap().as_slice(),
        ["WriteFile write_notes"],
        "the host was asked about the child's write"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "refused, so it never ran");
}

const READS_SECRETS: &str = r#"
api_version: "2"
name: reads
capabilities: [ReadFile]
agent: { tools: [read_secrets] }
execution: { limits: { model_turns: 4, tool_calls: 2 } }
workflow:
  - name: peek
    task: Peek.
"#;

#[tokio::test]
async fn an_agent_cannot_run_a_tool_whose_capability_its_name_hides() {
    // Named like a read, registered as network access: the name must not be
    // what authorizes it.
    let (tool, runs) = Counting::new("read_secrets", ToolCapability::NetworkAccess);
    let engine = Engine::builder()
        .model_provider(calls("read_secrets"))
        .tool(tool)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(READS_SECRETS).expect("loads");

    let _ = run(&harness).await;

    assert_eq!(runs.load(Ordering::SeqCst), 0, "the tool must not run");
}

const AGENT_WITH_LIST: &str = r#"
api_version: "2"
name: agent-list
capabilities: [ReadFile, WriteFile]
agent: { tools: [read_notes] }
execution: { limits: { model_turns: 4, tool_calls: 4 } }
workflow:
  - name: work
    task: Look around.
"#;

#[tokio::test]
async fn an_agent_step_never_runs_a_tool_outside_its_allowlist() {
    let (read, read_runs) = Counting::new("read_notes", ToolCapability::ReadFile);
    let (write, write_runs) = Counting::new("write_notes", ToolCapability::WriteFile);
    let proposes_write = vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: "w1".into(),
            name: "write_notes".into(),
            arguments: "{}".into(),
        },
        ResponseEvent::Done,
    ];
    let provider = ScriptedMockProvider::new(vec![
        proposes_write,
        vec![
            ResponseEvent::TextDelta("gave up".into()),
            ResponseEvent::Done,
        ],
    ]);
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .tool(read)
        .tool(write)
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(AGENT_WITH_LIST)
        .expect("loads");

    let _ = run(&harness).await;

    assert_eq!(write_runs.load(Ordering::SeqCst), 0, "outside the list");
    assert_eq!(read_runs.load(Ordering::SeqCst), 0, "nobody asked for it");
}

const SHELLS: &str = r#"
api_version: "2"
name: shells
execution: { limits: { model_turns: 2 } }
gates:
  go: { command: [ls] }
workflow:
  - name: go
    verify: [go]
"#;

/// Characterization: the host provides no tools at all, so a step that
/// needs one fails instead of running something else.
#[tokio::test]
async fn a_capability_the_host_does_not_provide_fails_the_step() {
    let engine = builder().build().expect("an engine builds");
    let harness = engine.load_harness_from_str(SHELLS).expect("loads");

    let outcome = run(&harness).await;

    assert_ne!(outcome.conclusion, RunConclusion::Cancelled);
    assert!(
        !matches!(&outcome.result, Some(Ok(_))),
        "a step with no tool behind it cannot succeed: {:?}",
        outcome.result
    );
}
