// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// running agents on local models you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a request the server rejects as too long for its context is sent
//! once more, shrunk, instead of ending the run - and the shrunk history
//! becomes the thread's, so the next request does not overflow again.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_model::ResponseEvent;

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// Rejects the first `rejections` requests the way a brain server does when
/// the prompt does not fit, then answers "ok". Records every request's size.
struct TightServer {
    rejections: usize,
    sizes: Arc<Mutex<Vec<usize>>>,
}

#[async_trait]
impl sven_model::ModelProvider for TightServer {
    fn name(&self) -> &str {
        "tight"
    }
    fn model_name(&self) -> &str {
        "tight"
    }
    async fn complete(&self, req: sven_model::CompletionRequest) -> anyhow::Result<Stream> {
        let mut sizes = self.sizes.lock().unwrap();
        sizes.push(req.messages.len());
        if sizes.len() <= self.rejections {
            anyhow::bail!(
                "brain error 400 Bad Request: {{\"error\":{{\"code\":\"context_length_exceeded\",\
                 \"message\":\"the request needs 32841 tokens but this model's serving capacity \
                 is 32768 tokens\"}}}}"
            );
        }
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn run(rejections: usize) -> (Vec<UiEvent>, Vec<usize>) {
    let (events, sizes, _) = run_keeping_store(rejections).await;
    (events, sizes)
}

async fn run_keeping_store(
    rejections: usize,
) -> (Vec<UiEvent>, Vec<usize>, Arc<Mutex<ThreadStore>>) {
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        use sven_machines::machines::reactive_agent::CHAT_THREAD;
        let mut seeded = store.lock().unwrap();
        for i in 0..30 {
            seeded.append(
                CHAT_THREAD,
                sven_model::Message::user(format!("question {i}")),
            );
            seeded.append(
                CHAT_THREAD,
                sven_model::Message::assistant(format!("answer {i}")),
            );
        }
    }
    let turn = TurnExecutor::new(
        Arc::new(TightServer {
            rejections,
            sizes: Arc::clone(&sizes),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
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
    rt.post(Event::user_message("one more")).await;
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            Ok(ev) = rx.recv() => {
                let end = matches!(ev, UiEvent::TurnComplete | UiEvent::Aborted { .. });
                events.push(ev);
                if end { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    rt.abort();
    let sizes = sizes.lock().unwrap().clone();
    (events, sizes, store)
}

#[tokio::test]
async fn an_overflow_rejection_is_retried_once_with_a_shrunk_request() {
    let (events, sizes) = run(1).await;
    assert_eq!(sizes.len(), 2, "one rejected request, one retry: {sizes:?}");
    assert!(sizes[1] < sizes[0], "the retry is smaller: {sizes:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::TextDelta(t) if t == "ok")),
        "the retry was answered: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Warning(w) if w.contains("context"))),
        "the retry is announced: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::Error(_))),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_second_overflow_is_a_failure_not_another_retry() {
    let (events, sizes) = run(5).await;
    assert_eq!(sizes.len(), 2, "exactly one retry: {sizes:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(m) if m.contains("context_length_exceeded"))),
        "the failure names the cause: {events:?}"
    );
}

#[tokio::test]
async fn the_trimmed_history_becomes_the_threads_so_the_next_request_fits() {
    use sven_machines::machines::reactive_agent::CHAT_THREAD;
    let (_, sizes, store) = run_keeping_store(1).await;
    let store = store.lock().unwrap();
    let thread = store.snapshot(CHAT_THREAD);
    // The thread holds the retried request's history plus the answer, not
    // the 61 messages that overflowed.
    assert_eq!(thread.len(), sizes[1] + 1, "{sizes:?}");
    let compactions = store.compactions();
    assert_eq!(compactions.len(), 1, "recorded as a compaction");
    assert_eq!(
        compactions[0].strategy,
        sven_hsm::CompactionStrategyUsed::Emergency
    );
    assert!(compactions[0].tokens_after < compactions[0].tokens_before);
}
