// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: the executors activate the instructions a session reaches and
//! announce them to the model.
//!
//! A successful tool call that names a file an instruction covers is recorded
//! in the kernel as an activation, and a task that names such a file gets the
//! instruction announced in the very request that carries it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_executors::{CompositeExecutorBuilder, ThreadStore, ToolExecutor, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Effect, Event, Machine, MachineId,
    PermissionPolicy, Reaction, ToolCallId, ToolCapability,
};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_tool_registry::{ApprovalPolicy, Tool, ToolCall, ToolOutput, ToolRegistry};
use sven_turn::instructions::{Activator, InstructionCatalog};
use sven_workspace::{
    Activation, SharedAgents, SharedKnowledge, SharedSkills, SkillInfo, SvenSkillMeta,
};

fn auth_skill() -> SkillInfo {
    SkillInfo {
        command: "auth".to_string(),
        name: "auth".to_string(),
        description: "Work on authentication.".to_string(),
        version: None,
        skill_md_path: PathBuf::from("skills/auth/SKILL.md"),
        skill_dir: PathBuf::from("skills/auth"),
        content: "body".to_string(),
        sven_meta: Some(SvenSkillMeta {
            activation: Some(Activation {
                paths: vec!["src/auth/**".to_string()],
                ..Activation::default()
            }),
            ..SvenSkillMeta::default()
        }),
    }
}

fn activator() -> Arc<Activator> {
    Arc::new(Activator::new(
        InstructionCatalog::new(
            SharedSkills::new(vec![auth_skill()]),
            SharedAgents::empty(),
            SharedKnowledge::empty(),
        ),
        Some(PathBuf::from("/proj")),
        "agent".to_string(),
    ))
}

struct Reader;

#[async_trait]
impl Tool for Reader {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a file."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        if call.args["path"] == "/proj/missing.rs" {
            return ToolOutput::err(&call.id, "no such file");
        }
        ToolOutput::ok(&call.id, "contents")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum St {
    Top,
    Idle,
    Done,
}

/// Reads the file named by the first user message and finishes.
struct Reading {
    id: MachineId,
}

impl Machine for Reading {
    type State = St;
    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> St {
        St::Top
    }
    fn initial(&self) -> St {
        St::Idle
    }
    fn superstate(&self, _: St) -> St {
        St::Top
    }
    fn is_terminal(&self, s: St) -> bool {
        s == St::Done
    }
    fn dispatch_state(&mut self, s: St, e: &Event, _: &mut Context) -> Reaction<St> {
        match (s, e) {
            (St::Idle, Event::UserMessage { text, .. }) => {
                Reaction::Handled(vec![Effect::CallTool {
                    call_id: ToolCallId::new(),
                    name: "read_file".into(),
                    capability: ToolCapability::ReadFile,
                    args: json!({ "path": text }),
                }])
            }
            (St::Idle, Event::ToolSucceeded { .. } | Event::ToolFailed { .. }) => {
                Reaction::Transition {
                    target: St::Done,
                    effects: vec![],
                    rationale: "the call ended".into(),
                }
            }
            _ => Reaction::Ignored,
        }
    }
}

async fn read(path: &str) -> Vec<String> {
    let mut registry = ToolRegistry::new();
    registry.register(Reader);
    let runtime = ErasedRuntime::spawn(
        Box::new(Hsm::new(Reading {
            id: MachineId::new(),
        })),
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        ToolExecutor::unrestricted(Arc::new(registry)).with_instructions(Some(activator())),
        16,
    );
    runtime.post(Event::user_message(path)).await;
    runtime.wait_done().await;
    runtime
        .join()
        .await
        .expect("the run ends")
        .ctx
        .active_instructions
}

#[tokio::test]
async fn reading_a_file_an_instruction_covers_activates_it() {
    assert_eq!(read("/proj/src/auth/login.rs").await, ["skill:auth"]);
}

#[tokio::test]
async fn reading_an_unrelated_file_activates_nothing() {
    assert!(read("/proj/src/billing/x.rs").await.is_empty());
}

#[tokio::test]
async fn a_failed_read_activates_nothing() {
    assert!(read("/proj/missing.rs").await.is_empty());
}

/// A provider that records the request it is sent.
struct Recording {
    last: Arc<Mutex<Option<sven_model::CompletionRequest>>>,
}

#[async_trait]
impl sven_model::ModelProvider for Recording {
    fn name(&self) -> &str {
        "recording"
    }
    fn model_name(&self) -> &str {
        "recording"
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.last.lock().unwrap() = Some(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn request_for(task: &str) -> sven_model::CompletionRequest {
    let last = Arc::new(Mutex::new(None));
    let turn = TurnExecutor::new(
        Arc::new(Recording {
            last: Arc::clone(&last),
        }),
        None,
        Arc::new(ToolRegistry::new()),
        Arc::new(Mutex::new(ThreadStore::new())),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_instructions(Some(activator()));
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    rt.post(Event::user_message(task)).await;
    for _ in 0..200 {
        if last.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rt.abort();
    let sent = last.lock().unwrap().take().expect("the model was called");
    sent
}

#[tokio::test]
async fn a_task_that_names_a_covered_file_is_announced_in_its_own_request() {
    let sent = request_for("fix the bug in src/auth/login.rs").await;
    let suffix = sent.system_dynamic_suffix.expect("an announcement");
    assert!(
        suffix.contains("## Active Instructions") && suffix.contains("`auth`"),
        "{suffix}"
    );
}

#[tokio::test]
async fn a_task_that_names_nothing_covered_sends_no_announcement() {
    let sent = request_for("say hello").await;
    assert!(sent.system_dynamic_suffix.is_none());
}
