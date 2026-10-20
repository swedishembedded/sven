// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that end a run with an
// explicit, machine-readable reason for its clients. If your team needs
// expertise in unattended agents you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a turn the thinking watchdog cuts short is a failure that says why,
//! and a summarisation call is not cut short by the thinking-token cap.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompactionConfig, CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy};
use sven_hsm::{CompactionStrategyUsed, UiEvent};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_model::ResponseEvent;

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// Reasons at length, then answers; or, when `hang`, reasons and never ends.
struct Reasoner {
    thinking_chars: usize,
    hang: bool,
}

#[async_trait]
impl sven_model::ModelProvider for Reasoner {
    fn name(&self) -> &str {
        "reasoner"
    }
    fn model_name(&self) -> &str {
        "reasoner"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(8192)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(512)
    }
    async fn complete(&self, _req: sven_model::CompletionRequest) -> anyhow::Result<Stream> {
        use futures::StreamExt;
        let thinking = Ok(ResponseEvent::ThinkingDelta(
            "t".repeat(self.thinking_chars),
        ));
        if self.hang {
            return Ok(futures::stream::iter([thinking])
                .chain(futures::stream::pending())
                .boxed());
        }
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            thinking,
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

struct Harness {
    rt: ErasedRuntime,
    cancel: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

fn harness(
    model: Reasoner,
    store: Arc<Mutex<ThreadStore>>,
    compaction: Option<CompactionConfig>,
) -> Harness {
    let cancel = Arc::new(tokio::sync::Mutex::new(None));
    let mut turn = TurnExecutor::new(
        Arc::new(model),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        store,
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::clone(&cancel),
    )
    .with_turn_limits(sven_turn::TurnLimits::from(sven_turn::ThinkingBudget {
        max_thinking_tokens: Some(10),
        thinking_timeout_secs: None,
    }));
    if let Some(compaction) = compaction {
        turn = turn.with_compaction_config(compaction);
    }
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    Harness { rt, cancel }
}

async fn until_ended(rx: &mut tokio::sync::broadcast::Receiver<UiEvent>) -> Vec<UiEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            Ok(ev) = rx.recv() => {
                let end = matches!(ev, UiEvent::Aborted { .. } | UiEvent::TurnComplete);
                events.push(ev);
                if end { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    events
}

#[tokio::test]
async fn a_watchdog_abort_is_an_error_naming_the_cause_and_the_setting() {
    let h = harness(
        Reasoner {
            thinking_chars: 4000,
            hang: false,
        },
        Arc::new(Mutex::new(ThreadStore::new())),
        None,
    );
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    let events = until_ended(&mut rx).await;
    h.rt.abort();

    let error = events.iter().position(|e| {
        matches!(e, UiEvent::Error(m) if m.contains("thinking-token limit") && m.contains("agent.max_thinking_tokens"))
    });
    let aborted = events
        .iter()
        .position(|e| matches!(e, UiEvent::Aborted { .. }));
    assert!(
        matches!((error, aborted), (Some(e), Some(a)) if e < a),
        "an Error naming the limit and the setting precedes Aborted: {events:?}"
    );
}

#[tokio::test]
async fn a_user_cancel_is_not_an_error() {
    let h = harness(
        Reasoner {
            thinking_chars: 1,
            hang: true,
        },
        Arc::new(Mutex::new(ThreadStore::new())),
        None,
    );
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    // Wait for the turn to be in flight, then cancel it the way Ctrl+C does.
    let sender = loop {
        if let Some(tx) = h.cancel.lock().await.take() {
            break tx;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let _ = sender.send(());
    let events = until_ended(&mut rx).await;
    h.rt.abort();
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::Aborted { .. })),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::Error(_))),
        "{events:?}"
    );
}

#[tokio::test]
async fn the_thinking_cap_does_not_abort_a_summarisation_call() {
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        use sven_machines::machines::reactive_agent::CHAT_THREAD;
        let mut seeded = store.lock().unwrap();
        for i in 0..20 {
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
    let h = harness(
        Reasoner {
            thinking_chars: 4000,
            hang: false,
        },
        store,
        Some(CompactionConfig {
            threshold: 0.005,
            overhead_reserve: 0.0,
            keep_recent: 2,
            strategy: sven_turn::CompactionStrategy::Structured,
        }),
    );
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("one more")).await;
    let events = until_ended(&mut rx).await;
    h.rt.abort();
    let strategy = events.iter().find_map(|e| match e {
        UiEvent::ContextCompacted { strategy, .. } => Some(strategy.clone()),
        _ => None,
    });
    assert_eq!(
        strategy,
        Some(CompactionStrategyUsed::Structured),
        "the summary was written, not replaced by the emergency fallback: {events:?}"
    );
}

/// What the aborted turn was filed as in the conversation.
async fn cancelled_turn_marker(reason: Option<&str>) -> String {
    use sven_machines::machines::reactive_agent::CHAT_THREAD;
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let h = harness(
        Reasoner {
            thinking_chars: 1,
            hang: true,
        },
        Arc::clone(&store),
        None,
    );
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    let sender = loop {
        if let Some(tx) = h.cancel.lock().await.take() {
            break tx;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    if let Some(reason) = reason {
        store.lock().unwrap().set_stop_reason(reason);
    }
    let _ = sender.send(());
    until_ended(&mut rx).await;
    h.rt.abort();
    let thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    thread
        .iter()
        .filter_map(|m| match &m.content {
            sven_model::MessageContent::Text(t) if t.contains("[aborted:") => Some(t.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_stop_from_outside_labels_the_aborted_turn_with_its_reason() {
    let marker = cancelled_turn_marker(Some("repetition stop: same call 3 times")).await;
    assert!(
        marker.contains("[aborted: repetition stop: same call 3 times]"),
        "{marker}"
    );
}

#[tokio::test]
async fn a_persons_cancel_keeps_its_own_label() {
    let marker = cancelled_turn_marker(None).await;
    assert!(
        marker.contains("[aborted: turn cancelled by user]"),
        "{marker}"
    );
}
