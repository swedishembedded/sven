// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements coding agents that stay inside a small model
// context window for its clients. If your team needs expertise in running
// agents on local models you can procure our services by sending an email to
// info@swedishembedded.com.
//! Proactive compaction trusts what the server counted over its own estimate.
//!
//! The chars/4 estimate undercounts code by tens of percent, so a threshold
//! applied to the estimate alone fires after the window is already exceeded
//! (observed: a 27B served at 32768 tokens rejected a request of 32841 that
//! sven had estimated at about 75% of the budget).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompactionConfig, CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::UiEvent;
use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;

/// Answers "ok" and reports a prompt far larger than the estimate says.
struct UndercountedModel {
    requests: Arc<Mutex<usize>>,
}

#[async_trait]
impl sven_model::ModelProvider for UndercountedModel {
    fn name(&self) -> &str {
        "undercounted"
    }
    fn model_name(&self) -> &str {
        "undercounted"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(4096)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(512)
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.requests.lock().unwrap() += 1;
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("ok".into())),
            Ok(sven_model::ResponseEvent::Usage {
                input_tokens: 3000,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn turn_events(rx: &mut tokio::sync::broadcast::Receiver<UiEvent>) -> Vec<UiEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            Ok(ev) = rx.recv() => {
                let done = ev == UiEvent::TurnComplete;
                events.push(ev);
                if done { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    events
}

fn compacted(events: &[UiEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, UiEvent::ContextCompacted { .. }))
}

#[tokio::test]
async fn compaction_fires_on_the_server_count_when_the_estimate_is_far_below_it() {
    let requests = Arc::new(Mutex::new(0));
    // About 1000 estimated tokens of history: 31% of the usable budget.
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        use sven_machines::machines::reactive_agent::CHAT_THREAD;
        let mut seeded = store.lock().unwrap();
        for i in 0..10 {
            seeded.append(
                CHAT_THREAD,
                sven_model::Message::user(format!("{i} {}", "q".repeat(200))),
            );
            seeded.append(
                CHAT_THREAD,
                sven_model::Message::assistant(format!("{i} {}", "a".repeat(200))),
            );
        }
    }
    let turn = TurnExecutor::new(
        Arc::new(UndercountedModel {
            requests: Arc::clone(&requests),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        store,
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_compaction_config(CompactionConfig {
        // The estimate of the history is about 31% of the budget, the server's
        // count of the first request is about 85% of it.
        threshold: 0.5,
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

    rt.post(Event::user_message("first question")).await;
    let first = turn_events(&mut rx).await;
    assert!(
        !compacted(&first),
        "nothing is known about the undercount before the first answer: {first:?}"
    );

    rt.post(Event::user_message("second question")).await;
    let second = turn_events(&mut rx).await;
    rt.abort();
    assert!(
        compacted(&second),
        "the server counted 3000 tokens for a request estimated far lower; compaction must act on that: {second:?}"
    );
}
