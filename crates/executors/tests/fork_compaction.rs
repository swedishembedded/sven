// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// running agents on local models you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: compaction never loses the conversation.
//!
//! The model is asked, in a fork of the real conversation, for notes that
//! replace the history; a conversation too large for one fork is summarised
//! in pieces; a failed or empty reply is retried with reasoning off; only
//! when the model cannot produce notes at all is history dropped, and then
//! loudly. A response that spends its output budget thinking is retried once
//! after a compaction.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompactionConfig, CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, CompactionStrategyUsed, Context, Event,
    PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::{CompletionRequest, Message, ResponseEvent};

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

fn is_fork(req: &CompletionRequest) -> bool {
    req.messages
        .last()
        .and_then(Message::as_text)
        .is_some_and(|t| t == sven_turn::COMPACTION_PROMPT)
}

/// A server whose tokenizer is denser than sven's chars/4: it counts one
/// token per two characters, and rejects a request whose prompt plus the
/// output it asked for passes the window - the way brain words it.
fn dense_server(window: u32) -> Reply {
    Box::new(move |req, _| {
        let prompt: usize = req
            .messages
            .iter()
            .map(|m| m.as_text().map_or(10, str::len) / 2)
            .sum();
        let needs = prompt + req.max_output_tokens_override.unwrap_or(0) as usize;
        if needs > window as usize {
            return vec![ResponseEvent::Error(format!(
                "context_length_exceeded: the request needs {needs} tokens but this model's \
                 serving capacity is {window} tokens"
            ))];
        }
        if is_fork(req) {
            notes_listing_codewords(req)
        } else {
            text("done")
        }
    })
}

/// What a scripted server answers to one request.
type Reply = Box<dyn Fn(&CompletionRequest, usize) -> Vec<ResponseEvent> + Send + Sync>;

struct Scripted {
    window: u32,
    reply: Reply,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
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
        let n = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req.clone());
            requests.len()
        };
        let events = (self.reply)(&req, n);
        Ok(Box::pin(futures::stream::iter(
            events.into_iter().map(Ok::<_, anyhow::Error>),
        )))
    }
}

fn text(t: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(t.into()), ResponseEvent::Done]
}

struct Run {
    events: Vec<UiEvent>,
    requests: Vec<CompletionRequest>,
    thread: Vec<Message>,
}

/// Seed `exchanges` question/answer pairs of `chars` characters, then post
/// one more question and run the turn to its end.
async fn run(window: u32, exchanges: usize, chars: usize, reply: Reply) -> Run {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        let mut seeded = store.lock().unwrap();
        for i in 0..exchanges {
            seeded.append(
                CHAT_THREAD,
                Message::user(format!("question {i} CODEWORD-{i} {}", "q".repeat(chars))),
            );
            seeded.append(CHAT_THREAD, Message::assistant(format!("answer {i}")));
        }
    }
    let turn = TurnExecutor::new(
        Arc::new(Scripted {
            window,
            reply,
            requests: Arc::clone(&requests),
        }),
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
    let mut rx = rt.subscribe_observations();
    rt.post(Event::user_message("one more question")).await;
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            Ok(ev) = rx.recv() => {
                let done = matches!(ev, UiEvent::TurnComplete);
                events.push(ev);
                if done { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    rt.abort();
    let thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    let requests = requests.lock().unwrap().clone();
    Run {
        events,
        requests,
        thread,
    }
}

fn strategies(events: &[UiEvent]) -> Vec<CompactionStrategyUsed> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::ContextCompacted { strategy, .. } => Some(strategy.clone()),
            _ => None,
        })
        .collect()
}

fn warnings(events: &[UiEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

/// Notes that list every CODEWORD the fork request holds, as a model reading
/// the conversation would.
fn notes_listing_codewords(req: &CompletionRequest) -> Vec<ResponseEvent> {
    let mut words = Vec::new();
    for m in &req.messages {
        if let Some(t) = m.as_text() {
            for w in t.split_whitespace().filter(|w| w.starts_with("CODEWORD-")) {
                if !words.contains(&w.to_string()) {
                    words.push(w.to_string());
                }
            }
        }
    }
    text(&format!("NOTES {}", words.join(" ")))
}

#[tokio::test]
async fn the_fork_sends_the_real_conversation_and_the_notes_replace_it() {
    let r = run(
        8192,
        6,
        3000,
        Box::new(|req, _| {
            if is_fork(req) {
                notes_listing_codewords(req)
            } else {
                text("done")
            }
        }),
    )
    .await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    let fork = r.requests.iter().find(|q| is_fork(q)).expect("a fork");
    // The history went as messages, not as one text blob: every seeded
    // question is its own message ahead of the instruction.
    assert!(
        fork.messages
            .iter()
            .filter(|m| m.as_text().is_some_and(|t| t.starts_with("question")))
            .count()
            >= 6
    );
    assert_eq!(
        fork.enable_thinking,
        Some(false),
        "reasoning cannot eat the reply budget"
    );
    // The turn itself then ran on the notes alone, the task kept, and the
    // codewords survived.
    let after = r.requests.last().unwrap();
    assert!(!is_fork(after));
    let sent: String = after.messages.iter().filter_map(|m| m.as_text()).collect();
    assert!(sent.contains("NOTES"), "{sent}");
    for i in 0..6 {
        assert!(sent.contains(&format!("CODEWORD-{i}")), "lost CODEWORD-{i}");
    }
    assert!(
        after.messages.len() < 8,
        "history was replaced: {}",
        after.messages.len()
    );
}

#[tokio::test]
async fn an_empty_reply_is_retried_and_the_retry_is_still_a_summary_not_a_drop() {
    let r = run(
        8192,
        6,
        3000,
        Box::new(|req, n| {
            if is_fork(req) && req.enable_thinking == Some(false) {
                text("   ") // empty after reasoning-off
            } else if is_fork(req) {
                let _ = n;
                notes_listing_codewords(req)
            } else {
                text("done")
            }
        }),
    )
    .await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    let forks: Vec<_> = r.requests.iter().filter(|q| is_fork(q)).collect();
    assert_eq!(forks.len(), 2);
    assert_eq!(forks[0].enable_thinking, Some(false));
    assert_eq!(
        forks[1].enable_thinking, None,
        "the provider's default is the second try"
    );
    assert!(warnings(&r.events)
        .iter()
        .all(|w| !w.contains("WITHOUT a summary")));
}

#[tokio::test]
async fn when_the_model_cannot_write_notes_history_is_dropped_loudly_with_the_amount() {
    let r = run(
        8192,
        6,
        3000,
        Box::new(|req, _| if is_fork(req) { text("") } else { text("done") }),
    )
    .await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Emergency]);
    let loud = warnings(&r.events)
        .into_iter()
        .find(|w| w.contains("WITHOUT a summary"))
        .expect("a loud warning");
    assert!(loud.contains("tokens"), "{loud}");
    assert!(loud.chars().any(|c| c.is_ascii_digit()), "{loud}");
}

#[tokio::test]
async fn a_conversation_larger_than_one_fork_is_summarised_in_pieces_and_nothing_is_dropped() {
    // About 9k tokens of history in a 6k window: no single fork can hold it.
    let r = run(
        6144,
        12,
        3000,
        Box::new(|req, _| {
            if is_fork(req) {
                notes_listing_codewords(req)
            } else {
                text("done")
            }
        }),
    )
    .await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    let forks: Vec<_> = r.requests.iter().filter(|q| is_fork(q)).collect();
    assert!(forks.len() >= 2, "{} fork(s)", forks.len());
    for fork in &forks {
        let tokens = sven_turn::estimate_tokens(&fork.messages, 1.0);
        assert!(
            tokens + 1000 < 6144,
            "a fork of {tokens} tokens does not fit"
        );
    }
    let sent: String = r
        .requests
        .last()
        .unwrap()
        .messages
        .iter()
        .filter_map(|m| m.as_text())
        .collect();
    for i in 0..12 {
        assert!(sent.contains(&format!("CODEWORD-{i}")), "lost CODEWORD-{i}");
    }
    assert!(warnings(&r.events).iter().all(|w| !w.contains("WITHOUT")));
    let _ = r.thread;
}

#[tokio::test]
async fn a_response_that_spent_its_budget_thinking_is_retried_once_after_a_compaction() {
    let r = run(
        8192,
        3,
        100,
        Box::new(|req, n| {
            if is_fork(req) {
                notes_listing_codewords(req)
            } else if !req
                .messages
                .iter()
                .any(|m| m.as_text().is_some_and(|t| t.contains("NOTES")))
                && n < 10
            {
                // The first attempt: all reasoning, then the budget is gone.
                vec![
                    ResponseEvent::ThinkingDelta("hmm ".repeat(50)),
                    ResponseEvent::MaxTokens,
                    ResponseEvent::Done,
                ]
            } else {
                text("done")
            }
        }),
    )
    .await;
    let w = warnings(&r.events);
    assert!(
        w.iter()
            .any(|w| w.contains("ran out of") && w.contains("thinking tokens")),
        "{w:?}"
    );
    let last = r.requests.last().unwrap();
    assert!(!is_fork(last));
    assert!(last
        .messages
        .iter()
        .any(|m| m.as_text().is_some_and(|t| t.contains("NOTES"))));
    // The question was asked once, not twice.
    let asked = last
        .messages
        .iter()
        .filter(|m| m.as_text() == Some("one more question"))
        .count();
    assert_eq!(asked, 1);
    assert!(r.events.iter().any(|e| matches!(e, UiEvent::TurnComplete)));
}

#[tokio::test]
async fn a_fork_the_server_counts_larger_than_estimated_is_planned_again_smaller_not_dropped() {
    let r = run(8192, 8, 3000, dense_server(8192)).await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    let forks: Vec<_> = r.requests.iter().filter(|q| is_fork(q)).collect();
    assert!(forks.len() >= 2, "{} fork(s)", forks.len());
    assert!(
        forks[1].messages.len() < forks[0].messages.len(),
        "the second fork covers less"
    );
    assert!(warnings(&r.events).iter().all(|w| !w.contains("WITHOUT")));
}
