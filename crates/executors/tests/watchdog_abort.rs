// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that end a run with an
// explicit, machine-readable reason for its clients. If your team needs
// expertise in unattended agents you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a turn the thinking watchdog cuts short is a failure that says why,
//! a reasoning loop is asked again once before it is one, and a summarisation
//! call is not cut short by the thinking-token cap.

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
        // Words that never repeat a stretch: honest reasoning, not a loop.
        let mut text = String::new();
        let mut word = 0u32;
        while text.len() < self.thinking_chars {
            text.push_str(&format!("w{word:x} "));
            word += 1;
        }
        let thinking = Ok(ResponseEvent::ThinkingDelta(text));
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
    let limits = sven_turn::TurnLimits::from(sven_turn::ThinkingBudget {
        max_thinking_tokens: Some(10),
        thinking_timeout_secs: None,
    });
    harness_with_limits(model, store, compaction, limits)
}

fn harness_with_limits(
    model: impl sven_model::ModelProvider + 'static,
    store: Arc<Mutex<ThreadStore>>,
    compaction: Option<CompactionConfig>,
    limits: sven_turn::TurnLimits,
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
    .with_turn_limits(limits);
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
            output_reserve: 0.0,
            summary_tokens: 10_000,
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
        Some(CompactionStrategyUsed::Fork),
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

#[tokio::test]
async fn a_turn_still_running_at_its_deadline_is_cut_off_and_says_so() {
    let limits = sven_turn::TurnLimits::default().with_turn_decoding(&sven_vocab::TurnDecoding {
        deadline: Some(1),
        ..sven_vocab::TurnDecoding::default()
    });
    let h = harness_with_limits(
        Reasoner {
            thinking_chars: 1,
            hang: true,
        },
        Arc::new(Mutex::new(ThreadStore::new())),
        None,
        limits,
    );
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    let events = until_ended(&mut rx).await;
    h.rt.abort();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(m) if m.contains("deadline of 1 s"))),
        "{events:?}"
    );
    assert!(events.iter().any(|e| matches!(e, UiEvent::Aborted { .. })));
}

/// Loops in its reasoning on the first `loops` calls, then answers.
struct Looper {
    loops: usize,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl sven_model::ModelProvider for Looper {
    fn name(&self) -> &str {
        "looper"
    }
    fn model_name(&self) -> &str {
        "looper"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(8192)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(512)
    }
    async fn complete(&self, _req: sven_model::CompletionRequest) -> anyhow::Result<Stream> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let events: Vec<anyhow::Result<ResponseEvent>> = if call < self.loops {
            let paragraph = "Let me reconsider the order of the two writes once more, \
                             because the flag and the payload may race on this core. ";
            vec![Ok(ResponseEvent::ThinkingDelta(paragraph.repeat(60)))]
        } else {
            vec![
                Ok(ResponseEvent::TextDelta("ok".into())),
                Ok(ResponseEvent::Done),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn looper(loops: usize, store: Arc<Mutex<ThreadStore>>) -> Harness {
    let limits = sven_turn::TurnLimits::from(sven_turn::ThinkingBudget {
        max_thinking_tokens: None,
        thinking_timeout_secs: None,
    });
    let model = Looper {
        loops,
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    harness_with_limits(model, store, None, limits)
}

#[tokio::test]
async fn a_reasoning_loop_is_asked_again_once_and_the_turn_goes_on() {
    use sven_machines::machines::reactive_agent::CHAT_THREAD;
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let h = looper(1, Arc::clone(&store));
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    let events = until_ended(&mut rx).await;
    h.rt.abort();
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::TurnComplete)),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(_) | UiEvent::Aborted { .. })),
        "{events:?}"
    );
    let thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    let texts: Vec<String> = thread
        .iter()
        .filter_map(|m| match &m.content {
            sven_model::MessageContent::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("repeated")),
        "the model is told why it is asked again: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("reconsider the order")),
        "the looping reasoning is not kept: {texts:?}"
    );
}

#[tokio::test]
async fn a_reasoning_loop_on_the_retry_too_is_an_error() {
    let h = looper(2, Arc::new(Mutex::new(ThreadStore::new())));
    let mut rx = h.rt.subscribe_observations();
    h.rt.post(Event::user_message("go")).await;
    let events = until_ended(&mut rx).await;
    h.rt.abort();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(m) if m.contains("looping in its reasoning"))),
        "{events:?}"
    );
}
