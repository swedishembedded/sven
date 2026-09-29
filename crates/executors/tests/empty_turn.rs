// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! A provider that never produces usable content must fail the run loudly.
//!
//! Drives the full kernel - a reactive agent machine, the composite
//! executor, the turn executor - against providers whose every turn is empty
//! (no text) or empty in substance (whitespace only), and checks what reaches
//! the observation sink.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Context, Event, PermissionPolicy, UiEvent};
use sven_kernel::ErasedRuntime;
use tokio::sync::Mutex;

/// Streams only `Done` - no text, no tool calls - every turn. Used to
/// reproduce a provider that "succeeds" transport-wise but never actually
/// answers (e.g. a server that silently discards a request it can't serve).
struct EmptyProvider;

/// Streams only whitespace text and nothing else, every turn. Whitespace is
/// not content: the model said nothing, and the empty-turn accounting must
/// treat it identically to no text at all.
struct WhitespaceProvider;

#[async_trait]
impl sven_model::ModelProvider for WhitespaceProvider {
    fn name(&self) -> &str {
        "whitespace"
    }
    fn model_name(&self) -> &str {
        "whitespace"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta(" \n\t".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[async_trait]
impl sven_model::ModelProvider for EmptyProvider {
    fn name(&self) -> &str {
        "empty"
    }
    fn model_name(&self) -> &str {
        "empty"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> =
            vec![Ok(sven_model::ResponseEvent::Done)];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A provider that always returns an empty turn (no text, no tool calls)
/// must eventually fail the run loudly, not report a silent success.
///
/// This reproduces the "false success" bug: a server that admits a request
/// and then produces nothing (brain's mid-stream rejection collapsed to
/// exactly this shape before the SSE-error-frame fix). The first empty turn
/// is tolerated - `GeneratingAction::EmptyTurn` nudges the model to try
/// again - but `TurnComplete` must NOT appear after that first attempt, and
/// after `EMPTY_TURN_FAILURE_THRESHOLD` consecutive empty turns the executor
/// must emit `UiEvent::Error` before `UiEvent::TurnComplete`.
#[tokio::test]
async fn empty_provider_fails_loudly_instead_of_silent_success() {
    let (rt, events) = silent_provider_run(Arc::new(EmptyProvider)).await;
    assert_loud_empty_turn_failure(&events);
    rt.abort();
}

/// Same contract for a provider that streams whitespace: whitespace is not
/// content, so the identical loud-failure path applies.
#[tokio::test]
async fn whitespace_only_provider_fails_loudly_instead_of_silent_success() {
    let (rt, events) = silent_provider_run(Arc::new(WhitespaceProvider)).await;
    assert_loud_empty_turn_failure(&events);
    rt.abort();
}

/// Drives a reactive agent against a provider that never produces usable
/// content, collecting observations until the turn ends or the bound expires.
async fn silent_provider_run(
    provider: Arc<dyn sven_model::ModelProvider>,
) -> (ErasedRuntime, Vec<UiEvent>) {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_llm::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        provider,
        None,
        Arc::new(sven_tools::ToolRegistry::new()),
        store,
        call_id_to_thread,
        cancel_handle,
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

    let mut rt_obs_rx = rt.subscribe_observations();

    let sent = rt
        .sink()
        .emit(Event::UserMessage { text: "hi".into() })
        .await;
    assert!(sent, "UserMessage must be accepted by the kernel sink");

    // Collect every observation up to and including the terminal
    // TurnComplete, with a generous bound (the nudge path re-enters the
    // kernel for a second LLM call before the run actually ends).
    let mut events: Vec<UiEvent> = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            Ok(ev) = rt_obs_rx.recv() => {
                let is_turn_complete = ev == UiEvent::TurnComplete;
                events.push(ev);
                if is_turn_complete { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    (rt, events)
}

/// The shared contract: exactly one TurnComplete, an Error naming the
/// consecutive-empty-turn failure, and that Error before the TurnComplete.
fn assert_loud_empty_turn_failure(events: &[UiEvent]) {
    let turn_complete_count = events
        .iter()
        .filter(|e| **e == UiEvent::TurnComplete)
        .count();
    assert_eq!(
        turn_complete_count, 1,
        "TurnComplete must appear exactly once, only after the failure \
         threshold is hit - not after the first (tolerated) empty turn: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(msg) if msg.contains("consecutive attempts"))),
        "an Error describing the consecutive-empty-turn failure must be emitted: {events:?}"
    );
    let error_idx = events
        .iter()
        .position(|e| matches!(e, UiEvent::Error(_)))
        .expect("Error must be present");
    let turn_complete_idx = events
        .iter()
        .position(|e| *e == UiEvent::TurnComplete)
        .expect("TurnComplete must be present");
    assert!(
        error_idx < turn_complete_idx,
        "Error must be emitted before the terminal TurnComplete: {events:?}"
    );
}
