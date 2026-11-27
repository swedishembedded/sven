// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose recorded sessions can be
// trained on as they were sampled, for its clients. If your team needs
// expertise in agent trajectories as training data then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: every earlier answer goes back to the model as it arrived - each
//! call's argument text byte for byte (key order, whitespace, escapes, and a
//! call whose text is not JSON at all), its text and its reasoning untrimmed,
//! and the server's `sampled` record as it was sent - while a tool runs on the
//! parsed arguments, and a call that cannot be parsed runs nothing and is
//! answered with the reason.

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
/// The third call's arguments: not JSON (an invalid escape).
const ARGS_C: &str = "{\"path\": \"\\{x\"}";
/// The server's record of what it sampled, with its own spacing.
const SAMPLED: &str = "{\"token_ids\":\"AQID\", \"finish\":\"stop\"}";

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
                ResponseEvent::ToolCall {
                    index: 2,
                    id: "call_c".into(),
                    name: "echo".into(),
                    arguments: ARGS_C.into(),
                },
                ResponseEvent::Sampled(
                    serde_json::value::RawValue::from_string(SAMPLED.into()).unwrap(),
                ),
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

    /// The answer's text message, the reasoning and the record it carries.
    fn text_reasoning_sampled(&self) -> (&str, Option<&str>, Option<&str>) {
        self.history
            .iter()
            .find_map(|m| match &m.content {
                MessageContent::Text(t) if m.role == sven_model::Role::Assistant => Some((
                    t.as_str(),
                    m.reasoning.as_deref(),
                    m.sampled.as_deref().map(|r| r.get()),
                )),
                _ => None,
            })
            .expect("the answer's text is in the history")
    }

    /// What the tool results said, in order.
    fn results(&self) -> Vec<String> {
        self.history
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolResult { content, .. } => Some(format!("{content:?}")),
                _ => None,
            })
            .collect()
    }
}

async fn session() -> Outcome {
    let second = Arc::new(Mutex::new(None));
    let ran_on = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Echo(Arc::clone(&ran_on)));
    let registry = Arc::new(registry);
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let call_threads = Arc::new(Mutex::new(HashMap::new()));
    let turn = TurnExecutor::new(
        Arc::new(ScriptedModel {
            second: Arc::clone(&second),
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
async fn each_answer_goes_back_as_it_arrived() {
    let out = session().await;
    let generated_a = ARGS_A.concat();
    assert_eq!(out.arguments(), vec![generated_a.as_str(), ARGS_B, ARGS_C]);
    assert_eq!(
        out.text_reasoning_sampled(),
        (TEXT, Some(REASONING), Some(SAMPLED))
    );
    // The tools run on the parsed values; the call that cannot be parsed runs
    // nothing and is answered with why.
    assert_eq!(out.ran_on, vec![parsed(&generated_a), parsed(ARGS_B)]);
    let results = out.results();
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results[2].contains("not valid JSON"), "{results:?}");
}
