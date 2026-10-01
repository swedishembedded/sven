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
use sven_hsm::{CompactionStrategyUsed, Context, Event, PermissionPolicy, UiEvent};
use sven_kernel::ErasedRuntime;
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

/// A capped-capacity provider that records the exact request it receives
/// instead of rejecting/panicking, then streams "pong". Used to prove the
/// *dynamic* output-token budget: a small prompt against a small context
/// window with a same-sized configured output cap (the exact shape that
/// made the fixed-reservation gate reject a 2-token "hi" outright) must
/// both (a) pass the gate and (b) actually reach the provider with a
/// max_output_tokens_override scaled to the real prompt size, not the
/// full configured cap and not `None`.
struct RecordingCappedProvider {
    context_window: u32,
    max_output_tokens: u32,
    last_request: Arc<std::sync::Mutex<Option<sven_model::CompletionRequest>>>,
}

#[async_trait]
impl sven_model::ModelProvider for RecordingCappedProvider {
    fn name(&self) -> &str {
        "recording-capped"
    }
    fn model_name(&self) -> &str {
        "recording-capped"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.context_window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.max_output_tokens)
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.last_request.lock().unwrap() = Some(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CompositeExecutor + ReactiveAgentMachine full-kernel integration test
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reactive_agent_machine_routes_user_message_to_text_delta_on_obs_sink() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    // Build a TurnExecutor backed by PongProvider so the machine's
    // kind="turn" CallLlm effect is handled end-to-end without a real LLM.
    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(PongProvider),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
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

/// Streams one `TextDelta` and then hangs forever - never reaches `Done`,
/// never errors on its own. The only way this stream ends is by dropping it,
/// which is exactly what happens when `TurnExecutor`'s `tokio::select!`
/// resolves via the cancel branch. Used to prove cancellation mid-stream:
/// the partial text must survive even though the provider itself never
/// finishes.
struct HangingProvider;

#[async_trait]
impl sven_model::ModelProvider for HangingProvider {
    fn name(&self) -> &str {
        "hanging"
    }
    fn model_name(&self) -> &str {
        "hanging"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        let stream = futures::stream::unfold(0u8, move |state| async move {
            match state {
                0 => Some((
                    Ok(sven_model::ResponseEvent::TextDelta("partial reply".into())),
                    1,
                )),
                _ => {
                    // Hang until the caller drops this stream (i.e. cancels).
                    futures::future::pending::<()>().await;
                    unreachable!("pending() never resolves")
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

/// Cancelling a turn mid-stream (the same mechanism ESC/Ctrl+C//abort use
/// via `TurnExecutor::cancel_handle`) must: preserve the text streamed so
/// far, report it via `UiEvent::Aborted` (not `UiEvent::Error`), post
/// `Event::UserCancelled` (not `Event::LlmFailed`) so the machine returns
/// cleanly to `Idle` rather than a failure state, and never emit a
/// `UiEvent::TurnComplete` (superseded by `Aborted`, which every consumer
/// already treats as terminal on its own).
#[tokio::test]
async fn cancelling_mid_stream_preserves_partial_text_and_reports_aborted() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(HangingProvider),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        store,
        call_id_to_thread,
        Arc::clone(&cancel_handle),
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
            text: "ping".into(),
        })
        .await;
    assert!(sent, "UserMessage must be accepted by the kernel sink");

    // Wait for the partial text to actually land on the observation plane
    // before cancelling - this is the same event the forwarder task uses to
    // update the partial-text accumulator, in the same loop iteration, so
    // seeing it here guarantees the accumulator already has it too. Without
    // this synchronization the test would race the cancel against the
    // delta's delivery.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "TextDelta never arrived");
        tokio::select! {
            Ok(ev) = rt_obs_rx.recv() => {
                if ev == UiEvent::TextDelta("partial reply".into()) {
                    break;
                }
            }
            _ = tokio::time::sleep(remaining) => panic!("TextDelta never arrived"),
        }
    }

    // Fire the abort exactly as the TUI's `send_abort_signal` does: take and
    // drop the stored cancel sender.
    let sender = cancel_handle.lock().await.take();
    drop(sender);

    // Collect observations until Aborted (or the deadline).
    let mut events: Vec<UiEvent> = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            Ok(ev) = rt_obs_rx.recv() => {
                let is_aborted = matches!(ev, UiEvent::Aborted { .. });
                events.push(ev);
                if is_aborted { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }

    let aborted = events.iter().find_map(|ev| match ev {
        UiEvent::Aborted { partial_text } => Some(partial_text.clone()),
        _ => None,
    });
    assert_eq!(
        aborted.as_deref(),
        Some("partial reply"),
        "Aborted must carry the text streamed before cancellation: {events:?}"
    );
    assert!(
        !events.contains(&UiEvent::TurnComplete),
        "a cancelled turn must not also emit TurnComplete: {events:?}"
    );
    assert!(
        !events.iter().any(|ev| matches!(ev, UiEvent::Error(_))),
        "a user-initiated cancel must not be reported as an Error: {events:?}"
    );

    // The machine must have returned cleanly to Idle (via UserCancelled),
    // not gotten stuck or routed through a failure path.
    rt.wait_for_state("Idle").await;

    rt.abort();
}

/// A prompt that exceeds the model's known effective window must fail before
/// `ModelProvider::complete()` is ever called (`CappedProvider::complete`
/// panics if reached) — the request never leaves the process, exactly what
/// stops sven from building a request the server would reject after already
/// paying the connection/admission cost.
#[tokio::test]
async fn oversized_prompt_fails_before_any_network_call() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    // Mirrors brain's real default capacity (BRAIN_QWEN_CTX=2048) from the
    // false-success bug this whole workstream started from.
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(CappedProvider {
            context_window: 2048,
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
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
    let sent = rt
        .sink()
        .emit(Event::UserMessage { text: huge_prompt })
        .await;
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
        events
            .iter()
            .any(|e| matches!(e, UiEvent::Error(msg) if msg.contains("usable input budget"))),
        "an Error describing the budget rejection must be emitted: {events:?}"
    );
    // The rejection must name which provider/model it came from - without
    // this, "reduce context or raise the server's capacity" gives the user
    // no way to know *which* server to act on, especially once more than one
    // is configured (a `providers:` map, or a per-state model override).
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::Error(msg)
            if msg.contains("provider=capped") && msg.contains("model=capped"))),
        "the budget error must name the provider and model it was rejected against: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "the turn must still terminate (not hang) after the budget rejection: {events:?}"
    );

    rt.abort();
}

/// The exact regression this whole workstream started from: a 1024-token
/// context window, a 1024-token configured output cap - a fixed reservation
/// of the full cap leaves *zero* usable input budget, rejecting even a
/// 2-token "hi". The dynamic budget must let it through and must scale the
/// output-token request down from the full 1024 (there's no room for that
/// much output alongside the prompt) rather than sending the raw configured
/// cap unchanged or leaving it unset.
#[tokio::test]
async fn tiny_prompt_fits_a_small_window_with_a_full_size_output_cap() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let last_request = Arc::new(std::sync::Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(RecordingCappedProvider {
            context_window: 1024,
            max_output_tokens: 1024,
            last_request: Arc::clone(&last_request),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
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
    rt.abort();

    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::Error(_))),
        "a 2-token prompt must not be rejected by the budget gate: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::TextDelta(t) if t == "pong")),
        "the turn must actually reach the provider and stream its reply: {events:?}"
    );

    let req = last_request
        .lock()
        .unwrap()
        .clone()
        .expect("the provider must have received a request");
    let override_tokens = req
        .max_output_tokens_override
        .expect("a configured cap with a known window must produce a scaled override, not None");
    assert!(
        override_tokens > 0 && override_tokens < 1024,
        "the override must be scaled down from the full 1024-token cap to fit alongside the prompt, got {override_tokens}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Proactive compaction
// ─────────────────────────────────────────────────────────────────────────────

/// Records every request it receives and always replies "ok" - stands in for
/// both the compaction summarization call and the real turn, so a test can
/// distinguish "compaction never happened" from "compaction happened and the
/// real turn still went through afterwards" purely by call count and content.
struct CountingProvider {
    context_window: u32,
    max_output_tokens: u32,
    requests: Arc<std::sync::Mutex<Vec<sven_model::CompletionRequest>>>,
}

#[async_trait]
impl sven_model::ModelProvider for CountingProvider {
    fn name(&self) -> &str {
        "counting"
    }
    fn model_name(&self) -> &str {
        "counting"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.context_window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.max_output_tokens)
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        self.requests.lock().unwrap().push(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("ok".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A long-running conversation against a small, low-threshold-configured
/// model must trigger compaction before the next turn: the store's thread
/// shrinks, a ContextCompacted observation fires, and — critically — the
/// user's actual pending message still gets a real answer afterwards (this
/// is not just "the turn got rejected", compaction must let it proceed).
#[tokio::test]
async fn long_thread_triggers_compaction_before_the_next_turn() {
    use sven_executors::{CompactionConfig, CompositeExecutorBuilder};
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::machines::reactive_agent::CHAT_THREAD;
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    // Seed a long-ish history so the (very low, for a fast test) compaction
    // threshold is comfortably crossed without needing thousands of messages.
    {
        let mut s = store.lock().unwrap();
        for i in 0..20 {
            s.append(
                CHAT_THREAD,
                sven_model::Message::user(format!("question {i}")),
            );
            s.append(
                CHAT_THREAD,
                sven_model::Message::assistant(format!("answer {i}")),
            );
        }
    }
    let tokens_before_seed: usize = store
        .lock()
        .unwrap()
        .snapshot(CHAT_THREAD)
        .iter()
        .map(sven_model::Message::approx_tokens)
        .sum();

    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(CountingProvider {
            context_window: 2048,
            max_output_tokens: 512,
            requests: Arc::clone(&requests),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        call_id_to_thread,
        cancel_handle,
    )
    .with_compaction_config(CompactionConfig {
        // Deliberately tiny so this test triggers reliably off a short seed
        // history rather than needing thousands of messages to be sure of
        // crossing the threshold - exact threshold behavior is covered by
        // sven_model::budget's own unit tests.
        threshold: 0.02,
        overhead_reserve: 0.0,
        keep_recent: 2,
        strategy: sven_turn::CompactionStrategy::Structured,
    });
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
            text: "one more question".into(),
        })
        .await;
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
    rt.abort();

    let compacted_event = events.iter().find_map(|e| match e {
        UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            ..
        } => Some((*tokens_before, *tokens_after, strategy.clone())),
        _ => None,
    });
    let (tokens_before, tokens_after, strategy) =
        compacted_event.unwrap_or_else(|| panic!("ContextCompacted must fire: {events:?}"));
    // >= not == : the snapshot compaction ran against also includes the new
    // "one more question" turn, appended to the thread before compaction is
    // even considered (see execute()'s ordering).
    assert!(
        tokens_before >= tokens_before_seed,
        "tokens_before ({tokens_before}) must cover at least the seeded history ({tokens_before_seed})"
    );
    assert!(
        tokens_after < tokens_before,
        "compaction must actually shrink the thread"
    );
    assert_eq!(strategy, CompactionStrategyUsed::Structured);

    // Exactly two model calls: one to summarize, one for the real turn.
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "must be a summarization call followed by the real turn's call"
    );

    // The real turn's reply still reached the observation plane normally.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::TextDelta(t) if t == "ok")),
        "the user's actual pending message must still get answered after compaction: {events:?}"
    );

    // The store now holds the compacted history, not the original 40 turns.
    let final_thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    assert!(
        final_thread.len() < 40,
        "the stored thread must have actually shrunk, got {} messages",
        final_thread.len()
    );
}

/// A provider whose `complete()` always errors - used to prove the
/// emergency-compaction fallback: when the summarization call itself fails,
/// compaction must still succeed via the deterministic, model-free path
/// rather than leaving the thread oversized or hanging the turn.
struct AlwaysErrorsProvider {
    context_window: u32,
    max_output_tokens: u32,
}

#[async_trait]
impl sven_model::ModelProvider for AlwaysErrorsProvider {
    fn name(&self) -> &str {
        "always-errors"
    }
    fn model_name(&self) -> &str {
        "always-errors"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.context_window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.max_output_tokens)
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        anyhow::bail!("simulated provider failure")
    }
}

/// When the summarization model call itself fails, compaction must fall back
/// to `emergency_compact` (deterministic, no model call) rather than hanging
/// the turn or leaving the oversized thread in place. The turn as a whole
/// still ends in a (loud) failure here, since `AlwaysErrorsProvider` also
/// fails the real turn that follows - but the fallback compaction itself
/// must complete and be observable.
#[tokio::test]
async fn compaction_falls_back_to_emergency_when_the_summarization_call_fails() {
    use sven_executors::{CompactionConfig, CompositeExecutorBuilder};
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::machines::reactive_agent::CHAT_THREAD;
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    {
        let mut s = store.lock().unwrap();
        for i in 0..20 {
            s.append(
                CHAT_THREAD,
                sven_model::Message::user(format!("question {i}")),
            );
            s.append(
                CHAT_THREAD,
                sven_model::Message::assistant(format!("answer {i}")),
            );
        }
    }

    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(AlwaysErrorsProvider {
            context_window: 2048,
            max_output_tokens: 512,
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        call_id_to_thread,
        cancel_handle,
    )
    .with_compaction_config(CompactionConfig {
        threshold: 0.02,
        overhead_reserve: 0.0,
        keep_recent: 2,
        strategy: sven_turn::CompactionStrategy::Structured,
    });
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
            text: "one more question".into(),
        })
        .await;
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
    rt.abort();

    assert!(
        events.iter().any(|e| matches!(e, UiEvent::ContextCompacted { strategy, .. } if *strategy == CompactionStrategyUsed::Emergency)),
        "must fall back to emergency compaction when the summarization call errors: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "the turn must still terminate (not hang) even though every model call fails: {events:?}"
    );
    let final_thread = store.lock().unwrap().snapshot(CHAT_THREAD);
    assert!(
        final_thread.len() < 40,
        "the thread must have shrunk via the emergency path, got {} messages",
        final_thread.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache-total / capacity reporting on UiEvent::TokenUsage
// ─────────────────────────────────────────────────────────────────────────────

/// Streams a fixed reply plus a `Usage` chunk carrying `cache_read`/
/// `cache_write`, on every call - used to prove `cache_read_total`/
/// `cache_write_total` genuinely accumulate *across* turns (TurnExecutor's
/// job) rather than reset each call (what `stream_turn` alone would report).
struct UsageReportingProvider {
    context_window: u32,
    max_output_tokens: u32,
    cache_read_per_turn: u32,
    cache_write_per_turn: u32,
}

#[async_trait]
impl sven_model::ModelProvider for UsageReportingProvider {
    fn name(&self) -> &str {
        "usage-reporting"
    }
    fn model_name(&self) -> &str {
        "usage-reporting"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.context_window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.max_output_tokens)
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
            Ok(sven_model::ResponseEvent::TextDelta("ok".into())),
            Ok(sven_model::ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_read_tokens: self.cache_read_per_turn,
                cache_write_tokens: self.cache_write_per_turn,
                cost_usd: None,
            }),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// `cache_read_total`/`cache_write_total` must be the running sum across
/// every turn on the thread, and `max_tokens`/`max_output_tokens` must
/// reflect the model's actual known capacity - none of this came from
/// `stream_turn` itself (it always reported 0/unset before TurnExecutor
/// started filling these in).
#[tokio::test]
async fn token_usage_reports_cumulative_cache_totals_and_model_capacity() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};
    use sven_machines::ReactiveAgentMachine;

    let store = Arc::new(std::sync::Mutex::new(sven_executors::ThreadStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        sven_hsm::ToolCallId,
        (String, String),
    >::new()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(UsageReportingProvider {
            context_window: 100_000,
            max_output_tokens: 4096,
            cache_read_per_turn: 5,
            cache_write_per_turn: 3,
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
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

    async fn run_one_turn(
        rt: &ErasedRuntime,
        rx: &mut tokio::sync::broadcast::Receiver<UiEvent>,
        text: &str,
    ) -> Vec<UiEvent> {
        let sent = rt
            .sink()
            .emit(Event::UserMessage { text: text.into() })
            .await;
        assert!(sent, "UserMessage must be accepted by the kernel sink");
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::select! {
                Ok(ev) = rx.recv() => {
                    let done = ev == UiEvent::TurnComplete;
                    events.push(ev);
                    if done { break; }
                }
                _ = tokio::time::sleep(remaining) => break,
            }
        }
        events
    }

    let first = run_one_turn(&rt, &mut rt_obs_rx, "one").await;
    let second = run_one_turn(&rt, &mut rt_obs_rx, "two").await;
    rt.abort();

    fn usage(events: &[UiEvent]) -> (u32, u32, usize, usize) {
        events
            .iter()
            .find_map(|e| match e {
                UiEvent::TokenUsage {
                    cache_read_total,
                    cache_write_total,
                    max_tokens,
                    max_output_tokens,
                    ..
                } => Some((
                    *cache_read_total,
                    *cache_write_total,
                    *max_tokens,
                    *max_output_tokens,
                )),
                _ => None,
            })
            .expect("a TokenUsage event must have been observed")
    }

    let (read1, write1, max_tokens1, max_output1) = usage(&first);
    assert_eq!(
        read1, 5,
        "first turn: cache_read_total == this turn's cache_read"
    );
    assert_eq!(write1, 3);
    assert_eq!(
        max_tokens1, 100_000,
        "must reflect the model's real catalog context window, not 0"
    );
    assert_eq!(max_output1, 4096);

    let (read2, write2, ..) = usage(&second);
    assert_eq!(
        read2, 10,
        "second turn: cache_read_total must ACCUMULATE (5 + 5), not reset to 5"
    );
    assert_eq!(
        write2, 6,
        "second turn: cache_write_total must accumulate (3 + 3)"
    );
}
