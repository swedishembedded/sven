// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running language-model agents on
// models with very small context windows for its clients. If your team needs
// expertise in making a small model a usable agent then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: `agent.profile: compact` fixes what a session sends a model.
//!
//! Every request carries the profile's system prompt, the three compact tool
//! definitions and the conversation, and nothing else; the whole request of a
//! short task is under 600 estimated tokens. The tools are the real ones: a
//! model that calls them (here a scripted policy behind the provider seam, as
//! an external expert behind an OpenAI-compatible endpoint would) changes the
//! workspace, reads the result and answers. The trajectory names the profile
//! version that produced it.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_model::{CompletionRequest, MessageContent, ResponseEvent, Role};
use sven_model_mock::CallbackProvider;
use sven_sdk::config::{compact_profile, Config, RunProfile};
use sven_sdk::Engine;
use sven_turn::ledger::schema_tokens;

/// The most tokens (estimated as the ledger does) a request of the short task
/// below may cost.
const REQUEST_CEILING: u32 = 600;

fn call(id: &str, name: &str, args: &Value) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        },
        ResponseEvent::Done,
    ]
}

fn results(req: &CompletionRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::ToolResult { content, .. } => Some(content.to_string()),
            _ => None,
        })
        .collect()
}

/// The expert: write a file, read it back through the shell, then read it with
/// the file tool, and answer with what the last read returned.
fn expert(req: &CompletionRequest) -> Vec<ResponseEvent> {
    let seen = results(req);
    match seen.len() {
        0 => call(
            "c1",
            "write_file",
            &json!({"path": "note.txt", "text": "value=7\n"}),
        ),
        1 => call("c2", "shell", &json!({"shell_command": "cat note.txt"})),
        2 => call("c3", "read_file", &json!({"path": "note.txt"})),
        _ => {
            let value = if seen[2].contains("value=7") {
                "7"
            } else {
                "?"
            };
            vec![ResponseEvent::TextDelta(value.into()), ResponseEvent::Done]
        }
    }
}

fn request_tokens(req: &CompletionRequest) -> u32 {
    let tools: u32 = req.tools.iter().map(schema_tokens).sum();
    let messages: usize = req
        .messages
        .iter()
        .map(|m| m.as_text().map_or(0, str::len))
        .sum();
    tools + u32::try_from(messages.div_ceil(4)).unwrap()
}

#[tokio::test]
async fn a_compact_session_sends_the_fixed_request_and_runs_the_real_tools() {
    let workspace = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<CompletionRequest>::new()));
    let log = Arc::clone(&requests);
    let provider = CallbackProvider::new(move |req| {
        log.lock().unwrap().push(req.clone());
        expert(req)
    });
    let mut config = Config::default();
    config.agent.profile = RunProfile::Compact;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(provider) as Arc<_>)
        .project_root(workspace.path())
        .approvals(sven_sdk::ApprovalPolicy::Auto)
        .build()
        .expect("an engine builds");

    let mut agent = engine.agent("agent");
    let outcome = agent
        .send("Write value=7 to note.txt, then tell me the value in it.")
        .await
        .expect("the run completes");

    assert_eq!(outcome.reply, "7");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("note.txt")).unwrap(),
        "value=7\n"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4, "one request per model turn");
    for req in requests.iter() {
        let system: Vec<_> = req
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .collect();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0].as_text(), Some(compact_profile::SYSTEM_PROMPT));
        let mut names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["read_file", "shell", "write_file"]);
        let tokens = request_tokens(req);
        assert!(
            tokens <= REQUEST_CEILING,
            "a request of the short task costs ~{tokens} tokens, ceiling {REQUEST_CEILING}"
        );
    }

    let trajectory = agent.trajectory();
    assert_eq!(
        trajectory.agent.extra,
        Some(json!({"sven": {"profile": compact_profile::ID}}))
    );
}

/// Spec: a run that announces its deadline tells the model its time exactly
/// as a headless run with that timeout does, so an embedding application that
/// collects episodes sends the requests a CLI run sends: the notice ends the
/// user message and every tool result, and an earlier notice stays where it
/// was. A deadline alone announces nothing.
#[tokio::test]
async fn a_run_with_a_deadline_ends_each_request_with_the_time_notice() {
    let workspace = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<CompletionRequest>::new()));
    let log = Arc::clone(&requests);
    let provider = CallbackProvider::new(move |req| {
        log.lock().unwrap().push(req.clone());
        expert(req)
    });
    let mut config = Config::default();
    config.agent.profile = RunProfile::Compact;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(provider) as Arc<_>)
        .project_root(workspace.path())
        .approvals(sven_sdk::ApprovalPolicy::Auto)
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent
        .send_with(
            "Write value=7 to note.txt, then tell me the value in it.",
            sven_sdk::RunOptions::new()
                .deadline(std::time::Duration::from_secs(780))
                .announce_deadline(true),
        )
        .await
        .expect("the run completes");
    let notice = "Time: 0 of 13 min used, 13 min left; leave time to verify and finish.";
    let requests = requests.lock().unwrap();
    let text = |m: &sven_model::Message| match &m.content {
        MessageContent::ToolResult { content, .. } => content.to_string(),
        _ => m.as_text().unwrap_or_default().to_string(),
    };
    for req in requests.iter() {
        let last = req.messages.last().expect("a request has messages");
        assert!(text(last).ends_with(notice), "{:?}", text(last));
    }
    let last = requests.last().expect("requests were sent");
    let carrying = last
        .messages
        .iter()
        .filter(|m| m.role != Role::System && text(m).ends_with(notice))
        .count();
    assert_eq!(carrying, requests.len(), "every notice once sent stays");
    drop(requests);

    let quiet = Arc::new(Mutex::new(Vec::<CompletionRequest>::new()));
    let log = Arc::clone(&quiet);
    let provider = CallbackProvider::new(move |req| {
        log.lock().unwrap().push(req.clone());
        expert(req)
    });
    let mut config = Config::default();
    config.agent.profile = RunProfile::Compact;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(provider) as Arc<_>)
        .project_root(workspace.path())
        .approvals(sven_sdk::ApprovalPolicy::Auto)
        .build()
        .expect("an engine builds");
    engine
        .agent("agent")
        .send_with(
            "Write value=7 to note.txt, then tell me the value in it.",
            sven_sdk::RunOptions::new().deadline(std::time::Duration::from_secs(780)),
        )
        .await
        .expect("the run completes");
    let quiet = quiet.lock().unwrap();
    assert!(
        quiet
            .iter()
            .flat_map(|r| &r.messages)
            .all(|m| !text(m).contains("min used")),
        "a deadline alone announces nothing"
    );
}
