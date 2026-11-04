// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an embedded agent is given exactly the tools its application names.
//!
//! A framework that hands every agent a shell and a file writer unless told
//! otherwise makes the dangerous choice the default. An engine starts with no
//! built-in tools; an application opts into a preset or registers its own.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in least-privilege agent tooling then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    Engine, Toolset,
};

struct Echo;

#[async_trait::async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn kernel_capability(&self) -> sven_sdk::tool::ToolCapability {
        sven_sdk::tool::ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Repeat the input."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "echo")
    }
}

fn answer(text: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done]
}

/// The tool names the model was offered on the last request.
async fn offered(toolset: Option<Toolset>, extra: bool) -> Vec<String> {
    let provider = Arc::new(ScriptedMockProvider::new(vec![answer("ok")]));
    let mut builder = Engine::builder().model_provider(Arc::clone(&provider) as Arc<_>);
    if let Some(toolset) = toolset {
        builder = builder.toolset(toolset);
    }
    if extra {
        builder = builder.tool(Arc::new(Echo));
    }
    let engine = builder.build().expect("an engine builds");
    engine.agent("agent").send("hello").await.expect("a reply");
    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    let mut names: Vec<String> = seen.tools.iter().map(|t| t.name.clone()).collect();
    names.sort();
    names
}

#[tokio::test]
async fn an_engine_starts_with_no_built_in_tools() {
    assert_eq!(offered(None, false).await, Vec::<String>::new());
    assert_eq!(offered(None, true).await, vec!["echo".to_string()]);
}

#[tokio::test]
async fn the_coding_preset_can_read_write_and_run() {
    let names = offered(Some(Toolset::coding()), true).await;
    for want in ["read_file", "write_file", "edit_file", "shell", "echo"] {
        assert!(names.iter().any(|n| n == want), "{want} missing: {names:?}");
    }
}

#[tokio::test]
async fn the_research_preset_cannot_write() {
    let names = offered(Some(Toolset::research()), false).await;
    assert!(names.iter().any(|n| n == "read_file"), "{names:?}");
    for denied in ["write_file", "edit_file"] {
        assert!(
            !names.iter().any(|n| n == denied),
            "{denied} offered: {names:?}"
        );
    }
}

/// A closing question with alternatives is turned into an `ask_question`
/// call only when that tool exists (pinned by the reactive machine's own
/// tests); without it the answer stands.
#[tokio::test]
async fn a_question_is_not_routed_to_a_tool_the_agent_does_not_have() {
    let text = "Which framework would you like me to use?\n\n1. Axum\n2. Actix\n";
    let provider = Arc::new(ScriptedMockProvider::new(vec![answer(text)]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let reply = agent.send("pick one").await.expect("a reply").reply;
    assert!(reply.contains("Axum"), "{reply}");
}
