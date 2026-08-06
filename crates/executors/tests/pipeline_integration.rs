//! End-to-end kernel pipeline integration tests.
//!
//! These tests verify the complete path from a user message through the HSM
//! kernel, through the executor, and out to the observation plane as `UiEvent`s.
//!
//! They use only the in-process mock model so no network access is needed.
//! The goal is to prove that text deltas and `TurnComplete` reach the
//! observation sink correctly.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Context, ErasedRuntime, Event, PermissionPolicy, UiEvent};
use tokio::sync::Mutex;

// ─────────────────────────────────────────────────────────────────────────────
// PongProvider: mock ModelProvider for tests
// ─────────────────────────────────────────────────────────────────────────────

/// Streams "pong" then Done — used to test the TurnExecutor path.
struct PongProvider;

#[async_trait]
impl sven_model::ModelProvider for PongProvider {
    fn name(&self) -> &str {
        "pong"
    }
    fn model_name(&self) -> &str {
        "pong"
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
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A provider with a fixed, known-small context window (mirroring brain's
/// real default capacity) that PANICS if `complete()` is ever called. Used
/// to prove the prompt-size budget gate rejects an oversized request before
/// building it or making any network call — not merely before the response
/// arrives.
struct CappedProvider {
    context_window: u32,
}

#[async_trait]
impl sven_model::ModelProvider for CappedProvider {
    fn name(&self) -> &str {
        "capped"
    }
    fn model_name(&self) -> &str {
        "capped"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.context_window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(0)
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        panic!("complete() must never be called for a request the budget gate should reject");
    }
}

/// Streams only `Done` — no text, no tool calls — every turn. Used to
/// reproduce a provider that "succeeds" transport-wise but never actually
/// answers (e.g. a server that silently discards a request it can't serve).
struct EmptyProvider;

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

// ─────────────────────────────────────────────────────────────────────────────
// CompositeExecutor + ReactiveAgentMachine full-kernel integration test
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reactive_agent_machine_routes_user_message_to_text_delta_on_obs_sink() {
    use sven_core::ReactiveAgentMachine;
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

    // Build a TurnExecutor backed by PongProvider so the machine's
    // kind="turn" CallLlm effect is handled end-to-end without a real LLM.
    let store = Arc::new(std::sync::Mutex::new(sven_llm::ConversationStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(PongProvider),
        None,
        Arc::new(sven_tools::ToolRegistry::new()),
        store,
        call_id_to_thread,
        cancel_handle,
    );
    let executor = CompositeExecutorBuilder::default()
        .with_turn(turn_exec)
        .build();

    // The ErasedRuntime requires a `Box<dyn ErasedMachine>`, which is
    // `Hsm<M>` wrapped in a Box — exactly what the mode registry does.
    let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
    let rt = ErasedRuntime::spawn(
        machine,
        Context::new(),
        PermissionPolicy::builder().build(),
        executor,
        64,
    );

    // Subscribe to the runtime's own outward observation plane.
    let mut rt_obs_rx = rt.subscribe_observations();

    // Send the user message directly into the kernel queue.
    let sent = rt
        .sink()
        .emit(Event::UserMessage {
            text: "ping".into(),
        })
        .await;
    assert!(sent, "UserMessage must be accepted by the kernel sink");

    // Collect observations with a bounded wait (500 ms is enough for in-process mock).
    let mut events: Vec<UiEvent> = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
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

    assert!(
        events.contains(&UiEvent::TextDelta("pong".into())),
        "TextDelta('pong') must arrive via ReactiveAgentMachine → TurnExecutor path: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive after a full user turn: {events:?}"
    );

    // Clean up
    rt.abort();
}

/// A provider that always returns an empty turn (no text, no tool calls)
/// must eventually fail the run loudly, not report a silent success.
///
/// This reproduces the "false success" bug: a server that admits a request
/// and then produces nothing (brain's mid-stream rejection collapsed to
/// exactly this shape before the SSE-error-frame fix). The first empty turn
/// is tolerated — `GeneratingAction::EmptyTurn` nudges the model to try
/// again — but `TurnComplete` must NOT appear after that first attempt, and
/// after `EMPTY_TURN_FAILURE_THRESHOLD` consecutive empty turns the executor
/// must emit `UiEvent::Error` before `UiEvent::TurnComplete`.
#[tokio::test]
async fn empty_provider_fails_loudly_instead_of_silent_success() {
    use sven_core::ReactiveAgentMachine;
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

    let store = Arc::new(std::sync::Mutex::new(sven_llm::ConversationStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(EmptyProvider),
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
        .emit(Event::UserMessage {
            text: "hi".into(),
        })
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

    let turn_complete_count = events.iter().filter(|e| **e == UiEvent::TurnComplete).count();
    assert_eq!(
        turn_complete_count, 1,
        "TurnComplete must appear exactly once, only after the failure \
         threshold is hit — not after the first (tolerated) empty turn: {events:?}"
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

    rt.abort();
}

/// A prompt that exceeds the model's known effective window must fail before
/// `ModelProvider::complete()` is ever called (`CappedProvider::complete`
/// panics if reached) — the request never leaves the process, exactly what
/// stops sven from building a request the server would reject after already
/// paying the connection/admission cost.
#[tokio::test]
async fn oversized_prompt_fails_before_any_network_call() {
    use sven_core::ReactiveAgentMachine;
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

    let store = Arc::new(std::sync::Mutex::new(sven_llm::ConversationStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    // Mirrors brain's real default capacity (BRAIN_QWEN_CTX=2048) from the
    // false-success bug this whole workstream started from.
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(CappedProvider { context_window: 2048 }),
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

    // Comfortably over 2048 tokens at the chars/4 heuristic.
    let huge_prompt = "a".repeat(60_000);
    let sent = rt.sink().emit(Event::UserMessage { text: huge_prompt }).await;
    assert!(sent, "UserMessage must be accepted by the kernel sink");

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

    assert!(
        events.iter().any(|e| matches!(e, UiEvent::Error(msg) if msg.contains("usable input budget"))),
        "an Error describing the budget rejection must be emitted: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "the turn must still terminate (not hang) after the budget rejection: {events:?}"
    );

    rt.abort();
}
