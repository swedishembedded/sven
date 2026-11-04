// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that notice when a local
// model server changes under them for its clients. If your team needs
// expertise in self-hosted LLM infrastructure then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What the live model server says about itself reaches the session as
//! events: an adapter swap once, and a missing tool-calling capability once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_model::{AdapterRef, ModelChange};
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

struct Server {
    change: Mutex<Option<ModelChange>>,
    tools: Option<bool>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for Server {
    fn name(&self) -> &str {
        "local-server"
    }
    fn model_name(&self) -> &str {
        "served"
    }
    fn take_model_change(&self) -> Option<ModelChange> {
        self.change.lock().unwrap().take()
    }
    fn tool_support(&self) -> Option<bool> {
        self.tools
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<sven_model::ResponseStream> {
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

struct Noop;

#[async_trait::async_trait]
impl Tool for Noop {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> sven_tool_api::policy::ApprovalPolicy {
        sven_tool_api::policy::ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
        sven_tool_api::ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "")
    }
}

fn adapter(digest: &str) -> Option<AdapterRef> {
    Some(AdapterRef {
        id: "tuned".into(),
        digest: digest.into(),
    })
}

/// Runs `turns` user turns against `server` with one tool registered and
/// returns every observation.
async fn observe(server: Server, turns: usize) -> Vec<UiEvent> {
    let mut registry = sven_tool_registry::ToolRegistry::new();
    registry.register(Noop);
    let turn = TurnExecutor::new(
        Arc::new(server),
        None,
        Arc::new(registry),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    let mut rx = rt.subscribe_observations();
    let mut events = Vec::new();
    for turn in 0..turns {
        rt.post(Event::user_message(format!("ping {turn}"))).await;
        let bound = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(bound, rx.recv()).await {
                Ok(Ok(ev)) => {
                    let done = ev == UiEvent::TurnComplete;
                    events.push(ev);
                    if done {
                        break;
                    }
                }
                _ => panic!("turn {turn} did not complete: {events:?}"),
            }
        }
    }
    rt.abort();
    events
}

#[tokio::test]
async fn an_adapter_swap_seen_by_a_probe_is_reported_before_the_next_turn() {
    let events = observe(
        Server {
            change: Mutex::new(Some(ModelChange {
                model: "served".into(),
                from: adapter("d1"),
                to: adapter("d2"),
                context_window: Some(8192),
            })),
            tools: None,
        },
        2,
    )
    .await;
    let changes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, UiEvent::ServedModelChanged { .. }))
        .collect();
    assert_eq!(
        changes,
        [&UiEvent::ServedModelChanged {
            model: "served".into(),
            from: adapter("d1"),
            to: adapter("d2"),
            context_window: Some(8192),
        }]
    );
}

#[tokio::test]
async fn a_model_the_server_says_cannot_call_tools_is_warned_about_once() {
    let events = observe(
        Server {
            change: Mutex::new(None),
            tools: Some(false),
        },
        2,
    )
    .await;
    let warnings: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Warning(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("served") && warnings[0].contains("tool"),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn silence_about_tools_is_not_a_warning() {
    let events = observe(
        Server {
            change: Mutex::new(None),
            tools: None,
        },
        1,
    )
    .await;
    assert!(!events.iter().any(|e| matches!(e, UiEvent::Warning(_))));
}
