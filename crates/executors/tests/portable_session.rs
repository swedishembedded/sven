// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A whole agent turn behaves the same on every host.
//!
//! The production pieces, nothing standing in for them: the reactive agent
//! machine, the kernel's one consumer loop, the composite executor, the turn
//! executor streaming a model call and assembling its tool call, the tool
//! executor running the tool through the registry, and the result going
//! back to the model. Only the model is scripted.
//!
//! One body, run natively under tokio (`cargo test -p sven-executors --test
//! portable_session`) and as wasm32 on a JavaScript event loop (`make
//! test/wasm`). Both assert the same observable sequence, so a pass on both
//! is parity by construction.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_executors::{CompositeExecutorBuilder, ThreadStore, ToolExecutor, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, ToolCapability,
    UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_model::{CompletionRequest, MessageContent, ModelProvider, ResponseEvent, ResponseStream};
use sven_tool_api::{ApprovalPolicy, Tool};
use sven_tool_registry::ToolRegistry;
use sven_vocab::{ToolCall, ToolOutput};

/// Turn one asks for `echo`; turn two answers with what the tool returned.
struct ScriptedModel {
    calls: Arc<Mutex<u32>>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ModelProvider for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model_name(&self) -> &str {
        "scripted"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let turn = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        let events = if turn == 1 {
            vec![
                ResponseEvent::TextDelta("Let me check.".into()),
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"#.into(),
                },
                ResponseEvent::ToolCall {
                    index: 0,
                    id: String::new(),
                    name: String::new(),
                    arguments: r#""hello from the tool"}"#.into(),
                },
                ResponseEvent::Done,
            ]
        } else {
            // The second call must carry the tool's result back to the model.
            let result = req
                .messages
                .iter()
                .find_map(|message| match &message.content {
                    MessageContent::ToolResult { content, .. } => Some(format!("{content:?}")),
                    _ => None,
                })
                .unwrap_or_else(|| "NO TOOL RESULT".into());
            vec![
                ResponseEvent::TextDelta(format!("The tool said: {result}")),
                ResponseEvent::Done,
            ]
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

/// Answers with its argument.
struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Repeats its text."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let text = call
            .args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        ToolOutput::ok(call.id.clone(), text.to_string())
    }
}

/// What a reader sees of the turn, in order: tool calls, tool results, and
/// the final answer's text.
fn readable(events: &[UiEvent]) -> Vec<String> {
    let mut seen = Vec::new();
    let mut text = String::new();
    for event in events {
        match event {
            UiEvent::TextDelta(delta) => text.push_str(delta),
            UiEvent::ToolCallStarted(call) => {
                if !text.is_empty() {
                    seen.push(format!("text: {}", std::mem::take(&mut text)));
                }
                seen.push(format!("call: {} {}", call.name, call.args));
            }
            UiEvent::ToolCallFinished {
                tool_name,
                output,
                is_error,
                ..
            } => {
                seen.push(format!("result: {tool_name} {output} error={is_error}"));
            }
            UiEvent::TurnComplete => {
                if !text.is_empty() {
                    seen.push(format!("text: {}", std::mem::take(&mut text)));
                }
                seen.push("turn complete".into());
            }
            _ => {}
        }
    }
    seen
}

async fn a_turn_calls_a_tool_and_answers_with_its_result() {
    let calls = Arc::new(Mutex::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Echo);
    let registry = Arc::new(registry);
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let call_threads = Arc::new(Mutex::new(HashMap::new()));
    let turn = TurnExecutor::new(
        Arc::new(ScriptedModel {
            calls: Arc::clone(&calls),
        }),
        None,
        Arc::clone(&registry),
        Arc::clone(&store),
        Arc::clone(&call_threads),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let tools = ToolExecutor::with_shared_store(
        registry,
        HashSet::from([ToolCapability::ReadFile]),
        call_threads,
        store,
    );
    let executor = CompositeExecutorBuilder::default()
        .with_turn(turn)
        .with_tool_executor(tools)
        .build();
    let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
    let rt = ErasedRuntime::spawn(
        machine,
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        executor,
        64,
    );
    let mut observations = rt.subscribe_observations();
    assert!(
        rt.post(Event::user_message("what does the tool say?"))
            .await
    );

    let mut events = Vec::new();
    let collected = sven_rt::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(event) = observations.recv().await {
            let done = event == UiEvent::TurnComplete;
            events.push(event);
            if done {
                break;
            }
        }
    })
    .await;
    assert!(
        collected.is_ok(),
        "the turn did not complete: {:?}",
        readable(&events)
    );

    assert_eq!(
        readable(&events),
        vec![
            "text: Let me check.".to_string(),
            r#"call: echo {"text":"hello from the tool"}"#.to_string(),
            "result: echo hello from the tool error=false".to_string(),
            r#"text: The tool said: Text("hello from the tool")"#.to_string(),
            "turn complete".to_string(),
        ]
    );
    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "one model call for the tool, one for the answer"
    );
    rt.abort();
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn native() {
    a_turn_calls_a_tool_and_answers_with_its_result().await;
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
async fn browser() {
    a_turn_calls_a_tool_and_answers_with_its_result().await;
}
