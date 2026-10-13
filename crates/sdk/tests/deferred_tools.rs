// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: with deferred disclosure an agent's requests carry the definitions
//! of a few core tools, a tool is added to them when the model loads it, and
//! the loaded tools belong to the agent's suspended state.

use std::sync::Arc;

use sven_model::{CompletionRequest, ResponseEvent, ToolSchema};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::config::Config;
use sven_sdk::{AgentState, Engine, Toolset};

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done]
}

fn calls(id: &str, tool: &str, arguments: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: tool.into(),
            arguments: arguments.into(),
        },
        ResponseEvent::Done,
    ]
}

fn config(deferred: bool) -> Config {
    let mut config = Config::default();
    if deferred {
        config.tools.disclosure.mode = sven_bootstrap::Disclosure::Deferred;
    }
    config
}

fn engine(deferred: bool, scripts: Vec<Vec<ResponseEvent>>) -> (Engine, Arc<ScriptedMockProvider>) {
    // Skills, personas and memory come from the home directory; an empty one
    // keeps the tool set independent of who runs the test.
    let home = tempfile::tempdir().expect("a home directory").into_path();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    let provider = Arc::new(ScriptedMockProvider::new(scripts));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .config(config(deferred))
        .toolset(Toolset::coding())
        .build()
        .expect("an engine builds");
    (engine, provider)
}

fn last_request(provider: &ScriptedMockProvider) -> CompletionRequest {
    provider.last_request.lock().unwrap().clone().unwrap()
}

fn names(tools: &[ToolSchema]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

fn size(tools: &[ToolSchema]) -> usize {
    tools
        .iter()
        .map(|t| (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4))
        .sum()
}

#[tokio::test]
async fn a_deferred_request_carries_the_core_tools_only() {
    let (full, full_provider) = engine(false, vec![says("ok")]);
    full.agent("agent").send("hello").await.expect("a reply");
    let (deferred, deferred_provider) = engine(true, vec![says("ok")]);
    deferred
        .agent("agent")
        .send("hello")
        .await
        .expect("a reply");

    let full_tools = last_request(&full_provider).tools;
    let core_tools = last_request(&deferred_provider).tools;

    assert_eq!(
        names(&core_tools),
        [
            "edit_file",
            "find_file",
            "grep",
            "read_file",
            "shell",
            "todo",
            "tools",
            "write_file"
        ]
    );
    assert!(
        size(&core_tools) * 3 < size(&full_tools) * 2,
        "core-only {} tokens vs full {}",
        size(&core_tools),
        size(&full_tools)
    );
}

#[tokio::test]
async fn a_loaded_tool_joins_the_next_request_after_the_core_tools() {
    let (engine, provider) = engine(
        true,
        vec![
            calls(
                "l1",
                "tools",
                r#"{"action":"load","names":["web_fetch","web_search"]}"#,
            ),
            says("loaded"),
        ],
    );
    engine
        .agent("agent")
        .send("get a page")
        .await
        .expect("a reply");

    let tools = last_request(&provider).tools;
    let names = names(&tools);
    assert_eq!(
        names[names.len() - 2..],
        ["web_fetch", "web_search"],
        "loaded tools follow the core tools in load order: {names:?}"
    );
    assert_eq!(
        &names[..8],
        [
            "edit_file",
            "find_file",
            "grep",
            "read_file",
            "shell",
            "todo",
            "tools",
            "write_file"
        ]
    );
}

#[tokio::test]
async fn a_call_to_a_tool_that_was_never_loaded_is_refused_with_the_way_to_load_it() {
    let (engine, provider) = engine(
        true,
        vec![
            calls("c1", "web_fetch", r#"{"url":"https://example.com"}"#),
            says("understood"),
        ],
    );
    engine
        .agent("agent")
        .send("fetch it")
        .await
        .expect("a reply");

    let seen = format!("{:?}", last_request(&provider).messages);
    assert!(
        seen.contains("is not loaded") && seen.contains("action=load"),
        "the model must be told how to proceed: {seen}"
    );
}

#[tokio::test]
async fn the_loaded_tools_survive_suspending_and_resuming_the_agent() {
    let (engine, provider) = engine(
        true,
        vec![
            calls("l1", "tools", r#"{"action":"load","names":["web_search"]}"#),
            says("loaded"),
            says("still here"),
        ],
    );
    let mut agent = engine.agent("agent");
    agent.send("search the web").await.expect("a reply");

    let stored = serde_json::to_string(&agent.suspend()).expect("serializable");
    let state: AgentState = serde_json::from_str(&stored).expect("deserializable");
    let mut resumed = engine.resume(state).expect("a resumable state");
    resumed.send("go on").await.expect("a reply");

    let tools = last_request(&provider).tools;
    assert!(
        names(&tools).contains(&"web_search"),
        "a resumed agent keeps what it loaded: {:?}",
        names(&tools)
    );
}
