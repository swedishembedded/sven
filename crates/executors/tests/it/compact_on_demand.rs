// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a bounded
// model context window for its clients. If your team needs expertise in
// long-running agent sessions then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: compaction on request, and the summary a compaction leaves.
//!
//! A person can ask for the conversation to be compacted now
//! (`Event::CompactRequested`): the whole thread is summarised even though it
//! still fits, the notes replace it, `ContextCompacted` carries the notes as
//! the message the conversation stems from, and the answer ends like a turn
//! (`TurnComplete`). A thread too short to summarise is reported, not
//! compacted. Automatic compaction reports its notes the same way; an
//! emergency trim has none.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompactionConfig, CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, CompactionStrategyUsed, Context, Effect, Event,
    ObservationSink, PermissionPolicy, UiEvent,
};
use sven_kernel::{EffectExecutor, ErasedRuntime};
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::{CompletionRequest, Message, ResponseEvent};

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// How the scripted model answers the request for notes.
#[derive(Clone, Copy)]
enum Notes {
    /// "NOTES" and every codeword the conversation holds.
    Written,
    /// An error: no notes can be had.
    Refused,
    /// Never answers.
    Hangs,
}

struct Scripted {
    notes: Notes,
    window: u32,
}

fn is_fork(req: &CompletionRequest) -> bool {
    req.messages
        .last()
        .and_then(Message::as_text)
        .is_some_and(|t| t == sven_turn::COMPACTION_PROMPT)
}

#[async_trait]
impl sven_model::ModelProvider for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model_name(&self) -> &str {
        "scripted"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.window)
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<Stream> {
        if !is_fork(&req) {
            return Ok(Box::pin(futures::stream::iter(vec![
                Ok(ResponseEvent::TextDelta("done".into())),
                Ok(ResponseEvent::Done),
            ])));
        }
        match self.notes {
            Notes::Hangs => Ok(Box::pin(futures::stream::pending())),
            Notes::Refused => Ok(Box::pin(futures::stream::iter(vec![Ok(
                ResponseEvent::Error("the server is down".into()),
            )]))),
            Notes::Written => {
                let words: Vec<&str> = req
                    .messages
                    .iter()
                    .filter_map(Message::as_text)
                    .flat_map(str::split_whitespace)
                    .filter(|w| w.starts_with("CODEWORD-"))
                    .collect();
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(ResponseEvent::TextDelta(format!(
                        "NOTES {}",
                        words.join(" ")
                    ))),
                    Ok(ResponseEvent::Done),
                ])))
            }
        }
    }
}

/// `exchanges` question/answer pairs of about `chars` characters each.
fn seed(exchanges: usize, chars: usize) -> Vec<Message> {
    (0..exchanges)
        .flat_map(|i| {
            [
                Message::user(format!("question {i} CODEWORD-{i} {}", "q".repeat(chars))),
                Message::assistant(format!("answer {i}")),
            ]
        })
        .collect()
}

struct Session {
    rt: ErasedRuntime,
    store: Arc<Mutex<ThreadStore>>,
    events: tokio::sync::broadcast::Receiver<UiEvent>,
}

fn session(notes: Notes, window: u32, seeded: Vec<Message>) -> Session {
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        let mut thread = store.lock().unwrap();
        for m in seeded {
            thread.append(CHAT_THREAD, m);
        }
    }
    let turn = TurnExecutor::new(
        Arc::new(Scripted { notes, window }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_compaction_config(CompactionConfig {
        threshold: 0.5,
        overhead_reserve: 0.0,
        output_reserve: 0.0,
        summary_tokens: 10_000,
    });
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    let events = rt.subscribe_observations();
    Session { rt, store, events }
}

impl Session {
    /// The events up to and including the next `TurnComplete` or `Aborted`.
    async fn answer(&mut self) -> Vec<UiEvent> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            let over = matches!(ev, UiEvent::TurnComplete | UiEvent::Aborted { .. });
            seen.push(ev);
            if over {
                break;
            }
        }
        seen
    }

    /// The text of each message of the thread.
    /// Waits until the machine has entered `state` (and so has begun the
    /// effect of that transition, which blocks the run until it ends).
    async fn entered(&mut self, state: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            if matches!(&ev, UiEvent::Transition { to, .. } if to == state) {
                return;
            }
        }
        panic!("the machine never entered {state}");
    }

    fn thread(&self) -> Vec<String> {
        texts(&self.store.lock().unwrap().snapshot(CHAT_THREAD))
    }
}

fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| m.as_text().unwrap_or_default().to_string())
        .collect()
}

/// The `ContextCompacted` events among `events`: strategy and summary.
fn compactions(events: &[UiEvent]) -> Vec<(CompactionStrategyUsed, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::ContextCompacted {
                strategy, summary, ..
            } => Some((strategy.clone(), summary.clone())),
            _ => None,
        })
        .collect()
}

fn warnings(events: &[UiEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Warning(w) => Some(w.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_requested_compaction_replaces_a_thread_that_still_fits_and_reports_the_notes() {
    // Four short exchanges in an 8k window: far under any automatic limit.
    let before = seed(4, 100);
    let mut s = session(Notes::Written, 8192, before.clone());
    s.rt.post(Event::CompactRequested).await;
    let events = s.answer().await;

    let found = compactions(&events);
    let [(strategy, Some(summary))] = found.as_slice() else {
        panic!("one compaction carrying its summary, got {events:?}");
    };
    assert_eq!(*strategy, CompactionStrategyUsed::Fork);
    assert!(summary.starts_with("NOTES"), "{summary}");
    for i in 0..4 {
        assert!(summary.contains(&format!("CODEWORD-{i}")), "{summary}");
    }
    assert!(
        matches!(events.last(), Some(UiEvent::TurnComplete)),
        "the answer ends like a turn: {events:?}"
    );
    // The summary is the message the rest of the conversation stems from.
    let thread = s.thread();
    assert!(thread.len() < before.len(), "{} messages", thread.len());
    assert!(
        thread.iter().any(|t| t.contains(summary.as_str())),
        "the thread holds the reported summary"
    );
    // The machine is at rest again: the next request is accepted.
    s.rt.wait_for_state("Idle").await;
}

#[tokio::test]
async fn a_thread_too_short_to_summarise_is_reported_not_compacted() {
    let before = seed(1, 100);
    let mut s = session(Notes::Written, 8192, before.clone());
    s.rt.post(Event::CompactRequested).await;
    let events = s.answer().await;

    assert!(compactions(&events).is_empty(), "{events:?}");
    assert!(
        warnings(&events).iter().any(|w| w.contains("too short")),
        "{events:?}"
    );
    assert!(matches!(events.last(), Some(UiEvent::TurnComplete)));
    assert_eq!(s.thread(), texts(&before), "the thread is untouched");
    s.rt.wait_for_state("Idle").await;
}

#[tokio::test]
async fn an_emergency_trim_has_no_summary() {
    let mut s = session(Notes::Refused, 8192, seed(8, 100));
    s.rt.post(Event::CompactRequested).await;
    let events = s.answer().await;
    assert_eq!(
        compactions(&events),
        [(CompactionStrategyUsed::Emergency, None)]
    );
    assert!(matches!(events.last(), Some(UiEvent::TurnComplete)));
}

#[tokio::test]
async fn a_stopped_compaction_leaves_the_thread_and_ends_the_answer() {
    let before = seed(4, 100);
    let mut s = session(Notes::Hangs, 8192, before.clone());
    s.rt.post(Event::CompactRequested).await;
    s.entered("Compacting").await;

    // What `RuntimeHandle::cancel` does: stop the turn's work, tell the machine.
    s.rt.cancel_turn();
    s.rt.post(Event::UserCancelled).await;
    let events = s.answer().await;

    assert!(
        matches!(events.last(), Some(UiEvent::Aborted { .. })),
        "{events:?}"
    );
    assert!(compactions(&events).is_empty());
    assert_eq!(s.thread(), texts(&before), "the thread is untouched");
    s.rt.wait_for_state("Idle").await;
}

#[tokio::test]
async fn a_refused_request_is_only_a_notice_and_touches_nothing() {
    let before = seed(4, 100);
    let s = session(Notes::Written, 8192, before.clone());
    let mut executor = CompositeExecutorBuilder::default()
        .with_turn(TurnExecutor::new(
            Arc::new(Scripted {
                notes: Notes::Written,
                window: 8192,
            }),
            None,
            Arc::new(sven_tool_registry::ToolRegistry::new()),
            Arc::clone(&s.store),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(tokio::sync::Mutex::new(None)),
        ))
        .build();
    let obs = ObservationSink::new(16);
    let mut seen = obs.subscribe();
    executor
        .execute(
            Effect::CompactContext {
                thread: CHAT_THREAD.to_string(),
                refusal: Some("a turn is running".into()),
            },
            &s.rt.sink(),
            &obs,
        )
        .await;

    let Ok(UiEvent::Warning(notice)) = seen.try_recv() else {
        panic!("a warning saying why");
    };
    assert!(notice.contains("a turn is running"), "{notice}");
    assert!(
        seen.try_recv().is_err(),
        "no ending: the turn running is not over"
    );
    assert_eq!(s.thread(), texts(&before));
}

#[tokio::test]
async fn automatic_compaction_reports_its_notes_too() {
    // Six exchanges of 3000 characters overflow half of an 8k window.
    let mut s = session(Notes::Written, 8192, seed(6, 3000));
    s.rt.post(Event::user_message("one more question")).await;
    let events = s.answer().await;
    let found = compactions(&events);
    let [(CompactionStrategyUsed::Fork, Some(summary))] = found.as_slice() else {
        panic!("one fork compaction with its notes, got {events:?}");
    };
    assert!(summary.starts_with("NOTES"), "{summary}");
}
