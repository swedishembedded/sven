// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! A multi-stage task that was interrupted and is resumed from its durable
//! record: the constraints and the verified findings reach the model, the
//! request is smaller than a replay of the transcript, and the task finishes.

use std::sync::Arc;
use std::time::Duration;

use sven_bootstrap::{AssemblyMode, Config, RuntimeBuilder, RuntimeContext};
use sven_hsm::UiEvent;
use sven_model::{CompletionRequest, FunctionCall, Message, MessageContent, Role};
use sven_model_mock::ScriptedMockProvider;
use sven_turn::ledger::estimate_request;

const CONSTRAINT: &str = "Constraint: never edit files under vendor/. Goal: port the parser.";
const FACT: &str = "parse() in src/parser.rs";
const OPEN_ITEM: &str = "fix the tokenizer type error";

fn call(id: &str, name: &str, args: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        },
        reasoning: None,
    }
}

fn bulk(tag: &str, lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("{tag} line {n} of the recorded output"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stages one and two of the task, as the session store holds them after the
/// process was interrupted during stage three.
fn interrupted_session() -> Vec<Message> {
    vec![
        Message::user(CONSTRAINT),
        call("r1", "read_file", r#"{"path":"src/parser.rs"}"#),
        Message::tool_result("r1", bulk("parser", 500)),
        call(
            "m1",
            "memory",
            &format!(r#"{{"action":"set","key":"entry_point","value":"{FACT}"}}"#),
        ),
        Message::tool_result("m1", "ok"),
        call("r2", "read_file", r#"{"path":"src/tokenizer.rs"}"#),
        Message::tool_result("r2", bulk("tokenizer", 500)),
        call("s1", "shell", r#"{"command":"cargo test"}"#),
        Message::tool_result(
            "s1",
            format!(
                "[exit 101]\n{}\nerror[E0308]: mismatched types in tokenizer",
                bulk("test", 300)
            ),
        ),
        call("t1", "todo", r#"{"action":"write"}"#),
        Message::tool_result("t1", format!("1. [x] read the parser\n2. [ ] {OPEN_ITEM}")),
        Message::assistant("Stage two is done; the tokenizer has a type error."),
    ]
}

struct Resumed {
    request: CompletionRequest,
    finished_with: String,
}

async fn resume(mode: AssemblyMode, root: &std::path::Path) -> Resumed {
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    config.context.assembly.mode = mode;
    let provider = ScriptedMockProvider::always_text("All stages are done.");
    let last_request = provider.last_request.clone();
    let ctx = RuntimeContext {
        project_root: Some(root.to_path_buf()),
        ..RuntimeContext::default()
    };
    let (runtime, handle, _channels, _mcp, _mcp_events) =
        RuntimeBuilder::new(Arc::new(config), "chat")
            .with_runtime_context(ctx)
            .with_model_provider(Box::new(provider))
            .with_initial_history(interrupted_session())
            .build()
            .await
            .expect("the resumed session builds");

    let mut events = handle.subscribe_observations();
    handle
        .send_user_message("Continue with stage three.".into())
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(UiEvent::TurnComplete)) => break,
            Ok(Ok(_)) => {}
            other => panic!("the resumed task did not complete: {other:?}"),
        }
    }
    let finished_with = handle
        .history_snapshot()
        .iter()
        .rev()
        .find_map(|m| {
            (m.role == Role::Assistant)
                .then(|| m.as_text().map(str::to_string))
                .flatten()
        })
        .unwrap_or_default();
    runtime.abort();
    let request = last_request
        .lock()
        .unwrap()
        .clone()
        .expect("a request was made");
    Resumed {
        request,
        finished_with,
    }
}

fn sent_text(request: &CompletionRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|m| m.as_text().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_reconstructed_resume_keeps_the_constraints_and_findings_in_fewer_tokens() {
    let root = tempfile::tempdir().unwrap();
    let replay = resume(AssemblyMode::Off, root.path()).await;
    let rebuilt = resume(AssemblyMode::Reconstruct, root.path()).await;

    let text = sent_text(&rebuilt.request);
    for needle in [
        CONSTRAINT,
        "Continue with stage three.",
        FACT,
        OPEN_ITEM,
        "error[E0308]: mismatched types in tokenizer",
        "Stage two is done",
    ] {
        assert!(
            text.contains(needle),
            "the rebuilt request lacks {needle:?}"
        );
    }

    let replay_tokens = estimate_request(&replay.request).estimated_total;
    let rebuilt_tokens = estimate_request(&rebuilt.request).estimated_total;
    assert!(
        rebuilt_tokens < replay_tokens,
        "rebuilt {rebuilt_tokens} >= replay {replay_tokens}"
    );
    assert_eq!(rebuilt.finished_with, "All stages are done.");
    assert_eq!(replay.finished_with, "All stages are done.");
}

#[tokio::test]
async fn the_raw_results_of_a_reconstructed_session_can_still_be_expanded() {
    let root = tempfile::tempdir().unwrap();
    let rebuilt = resume(AssemblyMode::Reconstruct, root.path()).await;
    let text = sent_text(&rebuilt.request);
    let id = text
        .split("chunk c-")
        .nth(1)
        .map(|rest| format!("c-{}", &rest[..16]))
        .expect("the recap names a chunk");
    let store_root = root.path().join(".sven").join("context");
    let store = std::fs::read_dir(&store_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let index = std::fs::read_to_string(store.join("index.jsonl")).unwrap();
    assert!(
        index.contains(&id),
        "{id} is recorded in {}",
        store.display()
    );
    assert!(std::fs::read_dir(store.join("payloads")).unwrap().count() >= 3);
}
