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
    dense_server_counting(window, 2.0)
}

/// As [`dense_server`], with `chars_per_token` characters to a token.
fn dense_server_counting(window: u32, chars_per_token: f32) -> Reply {
    Box::new(move |req, _| {
        let prompt: usize = req
            .messages
            .iter()
            .map(|m| (m.as_text().map_or(10, str::len) as f32 / chars_per_token) as usize)
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
    tool_choice_none: bool,
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
    fn supports_tool_choice_none(&self) -> bool {
        self.tool_choice_none
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
    let mut seed = Vec::new();
    for i in 0..exchanges {
        seed.push(Message::user(format!(
            "question {i} CODEWORD-{i} {}",
            "q".repeat(chars)
        )));
        seed.push(Message::assistant(format!("answer {i}")));
    }
    run_seeded(window, seed, reply).await
}

/// As [`run`], from the conversation `seed`.
async fn run_seeded(window: u32, seed: Vec<Message>, reply: Reply) -> Run {
    run_full(window, seed, reply, true).await
}

/// A tool the session offers; the forks must offer it too, uncallable.
struct Lookup;

#[async_trait]
impl sven_tool_registry::Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }
    fn description(&self) -> &str {
        "Looks something up."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn default_policy(&self) -> sven_tool_registry::ApprovalPolicy {
        sven_tool_registry::ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> sven_hsm::ToolCapability {
        sven_hsm::ToolCapability::ReadFile
    }
    async fn execute(&self, call: &sven_tool_registry::ToolCall) -> sven_tool_registry::ToolOutput {
        sven_tool_registry::ToolOutput::ok(&call.id, "found")
    }
}

/// As [`run_seeded`], with a provider that does or does not take
/// `tool_choice: none`.
async fn run_full(window: u32, seed: Vec<Message>, reply: Reply, tool_choice_none: bool) -> Run {
    let config = CompactionConfig {
        threshold: 0.5,
        overhead_reserve: 0.0,
        output_reserve: 0.0,
        summary_tokens: 10_000,
    };
    run_configured(window, seed, reply, tool_choice_none, config).await
}

/// As [`run_full`], under the compaction settings `config`.
async fn run_configured(
    window: u32,
    seed: Vec<Message>,
    reply: Reply,
    tool_choice_none: bool,
    config: CompactionConfig,
) -> Run {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    {
        let mut seeded = store.lock().unwrap();
        for m in seed {
            seeded.append(CHAT_THREAD, m);
        }
    }
    let mut registry = sven_tool_registry::ToolRegistry::new();
    registry.register(Lookup);
    let turn = TurnExecutor::new(
        Arc::new(Scripted {
            tool_choice_none,
            window,
            reply,
            requests: Arc::clone(&requests),
        }),
        None,
        Arc::new(registry),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_compaction_config(config);
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
        let body = match &m.content {
            sven_model::MessageContent::ToolResult {
                content: sven_model::ToolResultContent::Text(t),
                ..
            } => Some(t.as_str()),
            _ => m.as_text(),
        };
        if let Some(t) = body {
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

#[tokio::test]
async fn the_budget_is_announced_once_as_its_own_event_not_a_warning() {
    let r = run(8192, 1, 100, Box::new(|_, _| text("done"))).await;
    let budgets: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            UiEvent::ContextBudget(b) => Some(*b),
            _ => None,
        })
        .collect();
    assert_eq!(budgets.len(), 1, "{:?}", r.events);
    assert_eq!(budgets[0].window, 8192);
    assert_eq!(budgets[0].notes_tokens, 2048);
    assert_eq!(budgets[0].thinking_cap, None);
    assert!(warnings(&r.events)
        .iter()
        .all(|w| !w.contains("context budget")));
}

#[tokio::test]
async fn a_prompt_the_server_rejects_as_too_long_is_compacted_by_fork_and_every_codeword_kept() {
    // The estimate puts the history near 2.8k tokens, below the trigger; the
    // server counts one token a character: 9k, over its 8k window.
    let r = run(8192, 6, 1500, dense_server_counting(8192, 1.0)).await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    assert!(warnings(&r.events).iter().all(|w| !w.contains("WITHOUT")));
    assert!(warnings(&r.events).iter().any(|w| w.contains("too long")));
    let last = r.requests.last().unwrap();
    assert!(!is_fork(last));
    let sent: String = last.messages.iter().filter_map(|m| m.as_text()).collect();
    for i in 0..6 {
        assert!(sent.contains(&format!("CODEWORD-{i}")), "lost CODEWORD-{i}");
    }
    assert_eq!(
        last.messages
            .iter()
            .filter(|m| m.as_text() == Some("one more question"))
            .count(),
        1,
        "the question is asked once"
    );
    assert!(r.events.iter().any(|e| matches!(e, UiEvent::TurnComplete)));
}

#[tokio::test]
async fn a_first_message_too_large_for_a_fork_is_clipped_for_it_not_dropped() {
    let mut seed = vec![Message::user(format!(
        "the task CODEWORD-T {}",
        "t".repeat(14_000)
    ))];
    for i in 0..6 {
        seed.push(Message::assistant(format!("answer {i} CODEWORD-{i}")));
        seed.push(Message::user(format!("question {i} {}", "q".repeat(1200))));
    }
    seed.push(Message::assistant("so far so good"));
    // Window 6144: the fork has room for about 3.9k tokens, the task alone is 3.5k.
    let r = run_seeded(
        6144,
        seed,
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
    assert!(warnings(&r.events).iter().all(|w| !w.contains("WITHOUT")));
    assert!(
        warnings(&r.events)
            .iter()
            .any(|w| w.contains("tokens of it were cut")),
        "{:?}",
        warnings(&r.events)
    );
    let last = r.requests.last().unwrap();
    let sent: String = last.messages.iter().filter_map(|m| m.as_text()).collect();
    assert!(sent.contains(&"t".repeat(14_000)), "the task is kept whole");
}

fn fork_replies() -> Reply {
    Box::new(|req, _| {
        if is_fork(req) {
            notes_listing_codewords(req)
        } else {
            text("done")
        }
    })
}

fn seed(exchanges: usize) -> Vec<Message> {
    (0..exchanges)
        .flat_map(|i| {
            [
                Message::user(format!("question {i} CODEWORD-{i} {}", "q".repeat(3000))),
                Message::assistant(format!("answer {i}")),
            ]
        })
        .collect()
}

#[tokio::test]
async fn the_fork_offers_the_sessions_tools_but_forbids_calling_them() {
    let r = run_full(8192, seed(6), fork_replies(), true).await;
    let fork = r.requests.iter().find(|q| is_fork(q)).expect("a fork");
    let main = r
        .requests
        .iter()
        .find(|q| !is_fork(q))
        .expect("a main request");
    assert!(!main.tools.is_empty());
    assert_eq!(
        fork.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        main.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "the same definitions in the same order, so the prompt prefix matches"
    );
    assert_eq!(fork.tool_choice, sven_model::ToolChoice::None);
    assert_eq!(main.tool_choice, sven_model::ToolChoice::Auto);
}

#[tokio::test]
async fn a_provider_that_cannot_forbid_tool_calls_gets_a_fork_without_tools() {
    let r = run_full(8192, seed(6), fork_replies(), false).await;
    let fork = r.requests.iter().find(|q| is_fork(q)).expect("a fork");
    assert!(fork.tools.is_empty());
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
}

#[tokio::test]
async fn a_burst_of_parallel_tool_results_larger_than_the_window_is_summarised_part_by_part() {
    // Twelve calls answered at once, ~800 tokens each: 9.6k tokens in a window
    // of 8k. The stored form is every call, then every result.
    let call = |i: usize| Message {
        role: sven_model::Role::Assistant,
        content: sven_model::MessageContent::ToolCall {
            tool_call_id: format!("c{i}"),
            function: sven_model::FunctionCall {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        },
        reasoning: None,
    };
    let mut burst = vec![Message::user("read the twelve files")];
    burst.extend((0..12).map(call));
    burst.extend((0..12).map(|i| {
        Message::tool_result(
            format!("c{i}"),
            format!("CODEWORD-{i} {}", "r".repeat(3200)),
        )
    }));
    burst.push(Message::assistant("read them all"));
    let r = run_seeded(8192, burst, fork_replies()).await;
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    assert!(warnings(&r.events).iter().all(|w| !w.contains("WITHOUT")));
    assert!(
        warnings(&r.events).iter().all(|w| !w.contains("were cut")),
        "no result is cut: part of the batch fits a fork"
    );
    let forks: Vec<_> = r.requests.iter().filter(|q| is_fork(q)).collect();
    assert!(forks.len() >= 2, "{} fork(s)", forks.len());
    let last = r.requests.last().unwrap();
    let sent: String = last
        .messages
        .iter()
        .filter_map(|m| match &m.content {
            sven_model::MessageContent::ToolResult {
                content: sven_model::ToolResultContent::Text(t),
                ..
            } => Some(t.as_str()),
            _ => m.as_text(),
        })
        .collect();
    for i in 0..12 {
        assert!(sent.contains(&format!("CODEWORD-{i}")), "lost CODEWORD-{i}");
    }
}

/// With the default threshold (98%) a thread no server has counted yet is
/// sized pessimistically for its response, and that size can pass the window
/// while the plain estimate is still under the compaction limit. The turn
/// must compact first, not fail for want of room.
#[tokio::test]
async fn a_request_under_the_limit_whose_response_sizing_overflows_is_compacted_not_refused() {
    let window = 5_000;
    let config = CompactionConfig {
        threshold: 0.98,
        overhead_reserve: 0.0,
        output_reserve: 0.02,
        summary_tokens: 1_000,
    };
    let seed = (0..6)
        .flat_map(|i| {
            [
                Message::user(format!("question {i} CODEWORD-{i} {}", "q".repeat(2000))),
                Message::assistant(format!("answer {i}")),
            ]
        })
        .collect();
    let r = run_configured(window, seed, fork_replies(), true, config).await;
    assert!(
        !r.events.iter().any(|e| matches!(e, UiEvent::Error(_))),
        "{:?}",
        r.events
    );
    assert_eq!(strategies(&r.events), [CompactionStrategyUsed::Fork]);
    let fork = r.requests.iter().find(|q| is_fork(q)).expect("a fork");
    assert!(
        fork.max_output_tokens_override.unwrap() > 0,
        "the fork asks for a reply"
    );
    assert!(!is_fork(r.requests.last().unwrap()), "the turn then ran");
}
