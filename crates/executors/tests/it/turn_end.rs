// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements agent runtimes whose frontends always know
// when an answer is over for its clients. If your team needs expertise in
// event-driven agent kernels then you can procure our services by sending an
// email to info@swedishembedded.com.

//! Spec: a surface sees exactly one `TurnComplete` per answer, after all of
//! it - also when the model is asked again after a transient failure.
//!
//! Before the machine owned the end of its turns, the failed attempt already
//! reported `TurnComplete`, so a surface closed the answer and the retried
//! reply arrived after it, into no answer at all.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Context, Event, PermissionPolicy, UiEvent};
use sven_kernel::ErasedRuntime;
use sven_model::ResponseEvent;
use tokio::sync::Mutex;

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// Fails its first request the way a busy server does, then answers.
struct BusyThenAnswers {
    calls: AtomicU32,
}

#[async_trait]
impl sven_model::ModelProvider for BusyThenAnswers {
    fn name(&self) -> &str {
        "busy"
    }
    fn model_name(&self) -> &str {
        "busy"
    }
    async fn complete(&self, _req: sven_model::CompletionRequest) -> anyhow::Result<Stream> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            anyhow::bail!("HTTP 503 Service Unavailable");
        }
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("answered".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test(start_paused = true)]
async fn a_retried_answer_ends_once_after_its_reply() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let provider = Arc::new(BusyThenAnswers {
        calls: AtomicU32::new(0),
    });
    let turn_exec = sven_executors::TurnExecutor::new(
        provider.clone(),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new())),
        Arc::default(),
        Arc::new(Mutex::new(None)),
    );
    let executor = CompositeExecutorBuilder::default()
        .with_turn(turn_exec)
        .build();
    let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
    let rt = ErasedRuntime::spawn(
        machine,
        Context::new(),
        PermissionPolicy::builder().build(),
        executor,
        64,
    );
    let mut obs = rt.subscribe_observations();
    assert!(rt.sink().emit(Event::user_message("hi")).await);

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
    while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, obs.recv()).await {
        events.push(ev);
    }
    rt.abort();

    assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "{events:?}");
    let ends: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| **e == UiEvent::TurnComplete)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(ends.len(), 1, "exactly one end of the answer: {events:?}");
    let reply = events
        .iter()
        .position(|e| matches!(e, UiEvent::TextDelta(t) if t.contains("answered")))
        .expect("the retried reply is observed");
    assert!(
        reply < ends[0],
        "the answer ends after its reply: {events:?}"
    );
}
