// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose recorded sessions can be
// trained on as they were sampled, for its clients. If your team needs
// expertise in agent trajectories as training data then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: with `history_as_generated` on, every earlier answer goes back to the
//! model as the model wrote it - each call's argument text byte for byte (key
//! order, whitespace, escapes), its text and its reasoning untrimmed - while
//! the tool still runs on the parsed arguments. Off (the default), history is
//! sent exactly as before: arguments re-serialised compact with sorted keys,
//! reasoning trimmed.

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
use sven_model::{
    CompletionRequest, Message, MessageContent, ModelProvider, ResponseEvent, ResponseStream,
};
use sven_tool_api::{ApprovalPolicy, Tool};
use sven_tool_registry::ToolRegistry;
use sven_vocab::{ToolCall, ToolOutput};

/// Reasoning with the surrounding whitespace a model really sends.
const REASONING: &str = "\n  I will look twice.  \n";
/// Text before the calls, trailing space and all.
const TEXT: &str = "Let me check. ";
/// The first call's arguments, streamed in two pieces: keys out of order,
/// spaces around separators, a newline, unicode and escapes.
const ARGS_A: [&str; 2] = [
    "{ \"text\" : \"h\u{e9} \\\"q\\\" \\n\u{1f600}\",\n  ",
    "\"b\": [1, 2.50] }",
];
/// The second call's arguments: keys in reverse alphabetical order.
const ARGS_B: &str = "{\"z\": 1, \"a\": {\"y\": null, \"x\": true}}";

/// Turn one says something, reasons, and makes two calls; turn two records
/// the history it is sent and answers.
struct ScriptedModel {
    second: Arc<Mutex<Option<Vec<Message>>>>,
}

#[async_trait]
impl ModelProvider for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model_name(&self) -> &str {
        "scripted"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let first = req
            .messages
            .iter()
            .all(|m| !matches!(m.content, MessageContent::ToolResult { .. }));
        let events = if first {
            vec![
                ResponseEvent::ThinkingDelta(REASONING.into()),
                ResponseEvent::TextDelta(TEXT.into()),
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "call_a".into(),
                    name: "echo".into(),
                    arguments: ARGS_A[0].into(),
                },
                ResponseEvent::ToolCall {
                    index: 0,
                    id: String::new(),
                    name: String::new(),
                    arguments: ARGS_A[1].into(),
                },
                ResponseEvent::ToolCall {
                    index: 1,
                    id: "call_b".into(),
                    name: "echo".into(),
                    arguments: ARGS_B.into(),
                },
                ResponseEvent::Done,
            ]
        } else {
            *self.second.lock().unwrap() = Some(req.messages.clone());
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done]
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

/// Records the arguments it is run with.
struct Echo(Arc<Mutex<Vec<Value>>>);

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Records its arguments."
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
        self.0.lock().unwrap().push(call.args.clone());
        ToolOutput::ok(call.id.clone(), "ok")
    }
}

/// What one session sent back of its first answer, and what the tool ran on.
struct Outcome {
    history: Vec<Message>,
    ran_on: Vec<Value>,
}

impl Outcome {
    fn arguments(&self) -> Vec<&str> {
        self.history
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolCall { function, .. } => Some(function.arguments.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The answer's text message and the reasoning it carries.
    fn text_and_reasoning(&self) -> (&str, Option<&str>) {
        self.history
            .iter()
            .find_map(|m| match &m.content {
                MessageContent::Text(t) if m.role == sven_model::Role::Assistant => {
                    Some((t.as_str(), m.reasoning.as_deref()))
                }
                _ => None,
            })
            .expect("the answer's text is in the history")
    }
}

async fn session(as_generated: Option<bool>) -> Outcome {
    let second = Arc::new(Mutex::new(None));
    let ran_on = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Echo(Arc::clone(&ran_on)));
    let registry = Arc::new(registry);
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let call_threads = Arc::new(Mutex::new(HashMap::new()));
    let mut turn = TurnExecutor::new(
        Arc::new(ScriptedModel {
            second: Arc::clone(&second),
        }),
        None,
        Arc::clone(&registry),
        Arc::clone(&store),
        Arc::clone(&call_threads),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    if let Some(on) = as_generated {
        turn = turn.with_history_as_generated(on);
    }
    let tools = ToolExecutor::with_shared_store(
        registry,
        HashSet::from([ToolCapability::ReadFile]),
        call_threads,
        store,
    );
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        CompositeExecutorBuilder::default()
            .with_turn(turn)
            .with_tool_executor(tools)
            .build(),
        64,
    );
    let mut observations = rt.subscribe_observations();
    assert!(rt.post(Event::user_message("go")).await);
    let finished = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(event) = observations.recv().await {
            if event == UiEvent::TurnComplete {
                break;
            }
        }
    })
    .await;
    rt.abort();
    assert!(finished.is_ok(), "the session did not finish");
    let history = second
        .lock()
        .unwrap()
        .take()
        .expect("the model was called a second time");
    let ran_on = ran_on.lock().unwrap().clone();
    Outcome { history, ran_on }
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn history_as_generated_sends_each_answer_back_as_the_model_wrote_it() {
    let out = session(Some(true)).await;
    let generated_a = ARGS_A.concat();
    assert_eq!(out.arguments(), vec![generated_a.as_str(), ARGS_B]);
    assert_eq!(out.text_and_reasoning(), (TEXT, Some(REASONING)));
    // The tool runs on the parsed value either way.
    assert_eq!(out.ran_on, vec![parsed(&generated_a), parsed(ARGS_B)]);
}

/// Golden: the default sends history exactly as sven always has.
#[tokio::test]
async fn by_default_history_is_sent_re_serialised_as_before() {
    for out in [session(None).await, session(Some(false)).await] {
        assert_eq!(
            out.arguments(),
            vec![
                r#"{"b":[1,2.5],"text":"hé \"q\" \n😀"}"#,
                r#"{"a":{"x":true,"y":null},"z":1}"#,
            ]
        );
        assert_eq!(out.text_and_reasoning(), (TEXT, Some("I will look twice.")));
        assert_eq!(out.ran_on, vec![parsed(&ARGS_A.concat()), parsed(ARGS_B)]);
    }
}
