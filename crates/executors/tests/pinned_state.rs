// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep their goal and plan intact
// through long runs on small context windows for its clients. If your team
// needs expertise in agent memory you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: the goal and the todo list are state outside the conversation; a
//! compaction puts them back verbatim, whichever way it compacts.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompactionConfig, CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::ResponseEvent;
use sven_vocab::{TodoItem, TodoStatus};

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// Answers every request, the summarisation one included, with "ok", or
/// fails every request (so that only the emergency fallback is left).
struct Answers {
    fails: bool,
}

#[async_trait]
impl sven_model::ModelProvider for Answers {
    fn name(&self) -> &str {
        "answers"
    }
    fn model_name(&self) -> &str {
        "answers"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(8192)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(512)
    }
    async fn complete(&self, _req: sven_model::CompletionRequest) -> anyhow::Result<Stream> {
        if self.fails {
            anyhow::bail!("down");
        }
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn thread_after_a_compaction(fails: bool) -> Vec<String> {
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        let mut s = store.lock().unwrap();
        s.append(
            CHAT_THREAD,
            sven_model::Message::user("GOAL: fix the retry test"),
        );
        for i in 0..20 {
            s.append(
                CHAT_THREAD,
                sven_model::Message::assistant(format!("answer {i}")),
            );
            s.append(
                CHAT_THREAD,
                sven_model::Message::user(format!("question {i}")),
            );
        }
        s.set_todos(vec![TodoItem {
            id: "7".into(),
            content: "Replace the sleep".into(),
            status: TodoStatus::InProgress,
        }]);
    }
    let turn = TurnExecutor::new(
        Arc::new(Answers { fails }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_compaction_config(CompactionConfig {
        threshold: 0.005,
        overhead_reserve: 0.0,
        keep_recent: 2,
        strategy: sven_turn::CompactionStrategy::Structured,
    });
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    let mut rx = rt.subscribe_observations();
    rt.post(Event::user_message("go on")).await;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            Ok(ev) = rx.recv() => {
                if matches!(ev, UiEvent::ContextCompacted { .. }) { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    rt.abort();
    let thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    thread
        .iter()
        .filter_map(|m| m.as_text().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_summary_compaction_puts_the_goal_and_the_todo_list_back_verbatim() {
    let thread = thread_after_a_compaction(false).await;
    assert!(
        thread.iter().any(|t| t == "GOAL: fix the retry test"),
        "{thread:?}"
    );
    assert!(
        thread
            .iter()
            .any(|t| t.contains("- [in_progress] 7: Replace the sleep")),
        "{thread:?}"
    );
}

#[tokio::test]
async fn an_emergency_compaction_does_too() {
    let thread = thread_after_a_compaction(true).await;
    assert!(
        thread.iter().any(|t| t == "GOAL: fix the retry test"),
        "{thread:?}"
    );
    assert!(
        thread
            .iter()
            .any(|t| t.contains("- [in_progress] 7: Replace the sleep")),
        "{thread:?}"
    );
}
