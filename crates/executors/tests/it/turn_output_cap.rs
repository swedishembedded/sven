// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A turn's output-token limit reaches the provider whatever the provider
//! knows about its own model.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;

/// A provider that knows neither its context window nor its output cap, and
/// records the request it is sent.
struct UnknownModel {
    last: Arc<Mutex<Option<sven_model::CompletionRequest>>>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for UnknownModel {
    fn name(&self) -> &str {
        "unknown"
    }
    fn model_name(&self) -> &str {
        "unknown"
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.last.lock().unwrap() = Some(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn the_output_cap_applies_when_the_context_window_is_unknown() {
    let last = Arc::new(Mutex::new(None));
    let turn = TurnExecutor::new(
        Arc::new(UnknownModel {
            last: Arc::clone(&last),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_turn_limits(sven_turn::TurnLimits::default().with_max_output_tokens(Some(123)));
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    rt.post(Event::user_message("ping")).await;
    for _ in 0..200 {
        if last.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rt.abort();
    let sent = last.lock().unwrap().take().expect("the model was called");
    assert_eq!(sent.max_output_tokens_override, Some(123));
}

/// A provider whose context window is `window` tokens, recording what it is
/// sent.
struct SmallWindow {
    window: u32,
    last: Arc<Mutex<Option<sven_model::CompletionRequest>>>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for SmallWindow {
    fn name(&self) -> &str {
        "small"
    }
    fn model_name(&self) -> &str {
        "small"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.window)
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.last.lock().unwrap() = Some(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A prompt that passes the input gate but, sized the way a thread's first
/// request is (its estimate raised by half), leaves nothing of the window for
/// the response is never sent asking for zero output tokens: every server
/// refuses that, so the turn fails saying the prompt leaves no room.
#[tokio::test]
async fn a_request_with_no_room_left_for_a_response_is_never_sent() {
    let last = Arc::new(Mutex::new(None));
    let turn = TurnExecutor::new(
        Arc::new(SmallWindow {
            window: 1500,
            last: Arc::clone(&last),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    // About 1100 estimated tokens: under the 1500-token window's input gate,
    // over what a first request's raised estimate leaves of it.
    rt.post(Event::user_message("word ".repeat(800))).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    rt.abort();
    let sent = last.lock().unwrap().take();
    assert!(
        sent.as_ref()
            .is_none_or(|r| r.max_output_tokens_override != Some(0)),
        "a request asking for no output tokens was sent: {:?}",
        sent.map(|r| r.max_output_tokens_override)
    );
}

/// The request a one-message turn sends to `model` under `limits`.
async fn first_request(
    model: Arc<dyn sven_model::ModelProvider>,
    last: &Arc<Mutex<Option<sven_model::CompletionRequest>>>,
    limits: sven_turn::TurnLimits,
    prompt: String,
) -> sven_model::CompletionRequest {
    let turn = TurnExecutor::new(
        model,
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_turn_limits(limits);
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    rt.post(Event::user_message(prompt)).await;
    for _ in 0..200 {
        if last.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rt.abort();
    let sent = last.lock().unwrap().take();
    sent.expect("the model was called")
}

/// A response bounded only by what the prompt leaves of the window carries
/// that bound as the room, not as a limit: the room is sven's estimate, and a
/// server that ends a reply at its own context knows it exactly. A limit
/// someone set stays a limit.
#[tokio::test]
async fn a_budget_bound_only_by_the_window_is_marked_as_the_room() {
    let last = Arc::new(Mutex::new(None));
    let window = || {
        Arc::new(SmallWindow {
            window: 8000,
            last: Arc::clone(&last),
        })
    };
    let roomy = first_request(window(), &last, Default::default(), "word ".repeat(2000)).await;
    assert!(
        roomy.output_limit_is_room,
        "{:?}",
        roomy.max_output_tokens_override
    );
    let capped = sven_turn::TurnLimits::default().with_max_output_tokens(Some(123));
    let limited = first_request(window(), &last, capped, "ping".into()).await;
    assert_eq!(limited.max_output_tokens_override, Some(123));
    assert!(!limited.output_limit_is_room);
}

/// A reasoning model in a 20 000-token window: every reply thinks at length
/// (about 10 000 tokens, kept with the message but never sent back) and the
/// server counts each request at a few hundred tokens.
struct Thinker {
    calls: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for Thinker {
    fn name(&self) -> &str {
        "thinker"
    }
    fn model_name(&self) -> &str {
        "thinker"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(20_000)
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.calls.lock().unwrap() += 1;
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            // Varied, so the repetition watchdog leaves it alone.
            Ok(sven_model::ResponseEvent::ThinkingDelta(
                (0..9_000).map(|i| format!("{i} ")).collect(),
            )),
            Ok(sven_model::ResponseEvent::TextDelta("ok".into())),
            Ok(sven_model::ResponseEvent::Usage {
                input_tokens: 400,
                output_tokens: 10_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// The input gate judges a request by what the server counted of the
/// conversation, not by reasoning the conversation stores and never sends:
/// a live run was refused at an estimated 1 069 531 tokens when the server's
/// last count was 89 315.
#[tokio::test]
async fn reasoning_that_is_never_sent_does_not_fill_the_context() {
    let calls = Arc::new(Mutex::new(0));
    let turn = TurnExecutor::new(
        Arc::new(Thinker {
            calls: Arc::clone(&calls),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    for turn in 1..=4 {
        rt.post(Event::user_message("next")).await;
        for _ in 0..300 {
            if *calls.lock().unwrap() >= turn {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    rt.abort();
    assert_eq!(*calls.lock().unwrap(), 4, "a turn was refused as too large");
}
