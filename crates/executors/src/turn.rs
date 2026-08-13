// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-turn effect executor — the kernel-native turn engine.
//!
//! Handles `Effect::CallLlm { request: {"kind": "turn", ...} }` emitted by
//! any machine that uses [`crate::loop_core`].  For each turn:
//!
//! 1. Deserialises the [`TurnRequest`] from the effect payload.
//! 2. Loads a snapshot of the named thread from the shared [`ThreadStore`].
//! 3. Resolves the model provider (per-state override or default).
//! 4. Resolves tool schemas from the registry for the requested tool names.
//! 5. Calls [`sven_core::stream_turn`] which streams a single model response,
//!    collecting proposed tool calls (accumulation-only, no dispatch).
//! 6. Appends the assistant turn (text + tool-call messages) to the thread
//!    **append-only** (cache-safety invariant).
//! 7. Annotates each proposed tool call with its [`ToolCapability`] via
//!    `registry.capability_of`.
//! 8. Registers `call_id → thread` in the shared registry so
//!    [`ToolExecutor`] can append results to the right thread.
//! 9. Posts `Event::LlmTurnComplete { thread, text, tool_calls }` inward
//!    (or `Event::LlmFailed` on error), followed by `UiEvent::TurnComplete`
//!    on the outward plane.
//!
//! # TurnComplete ordering guarantee
//!
//! `UiEvent::TurnComplete` is emitted **after** the inward completion event
//! is already in the kernel queue, preventing the TUI from marking the turn
//! complete before the machine has transitioned.
//!
//! # Cancellation
//!
//! A shared cancel slot allows a frontend (Esc/Ctrl+C/`/abort` in the TUI, the
//! thinking-token/time watchdog, ACP's `cancel()`, ...) to abort an in-flight
//! turn. Cancellation is deliberately **not** reported as a failure: the text
//! streamed so far is teed into a local accumulator as it arrives (so it
//! survives the dropped `stream_turn` future), persisted to the thread as an
//! assistant message tagged `[aborted]` so the next turn knows it was cut
//! off, and surfaced as `Event::UserCancelled` + `UiEvent::Aborted
//! { partial_text }` rather than `Event::LlmFailed` + `UiEvent::Error`. This
//! matters because `LlmFailed` drives `SdlcMachine` into `Recovery`/`Failed`,
//! the right response to a *real* failure but the wrong one for a turn the
//! user deliberately cut short. `UiEvent::Aborted` is terminal on its own
//! (see every consumer: `RuntimeRunner`, `KernelAgent`, ACP's prompt loop),
//! so unlike the error path it is not followed by a separate `TurnComplete`.
//!
//! # Empty turns
//!
//! A turn with no text and no tool calls is tolerated once per thread -
//! `GeneratingAction::EmptyTurn` nudges the model to try again - but a
//! streak of [`EMPTY_TURN_FAILURE_THRESHOLD`] consecutive empty turns on the
//! same thread fails the turn instead of letting the run silently "succeed"
//! with no output. This guards against providers that admit a streaming
//! request (`HTTP 200` + SSE) and only discover mid-stream that it can't be
//! served, since headless mode otherwise treats `UiEvent::TurnComplete` as
//! "the run finished successfully" regardless of what it actually produced.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Consecutive empty turns (no text, no tool calls) on one thread before the
/// turn is failed outright instead of nudged again. See the module docs.
const EMPTY_TURN_FAILURE_THRESHOLD: u32 = 2;

// ─── Sync helpers ─────────────────────────────────────────────────────────────

/// Snapshot a thread from the store (non-async; guard is never held across await).
fn snapshot_thread(store: &Mutex<ThreadStore>, thread_id: &str) -> Vec<Message> {
    store
        .lock()
        .map(|s| s.snapshot(thread_id))
        .unwrap_or_default()
}

/// Append a batch of messages to a store thread (non-async).
fn append_messages(store: &Mutex<ThreadStore>, thread_id: &str, messages: Vec<Message>) {
    if let Ok(mut s) = store.lock() {
        for msg in messages {
            s.append(thread_id, msg);
        }
    }
}

/// Register call_id → (thread, original_id) in the registry (non-async).
///
/// The `original_id` is the raw tool_call_id string returned by the LLM (e.g.
/// `"call_HyZJn1bTtqVbmzzS3W4VM4xb"` for OpenAI).  It is preserved so that
/// `ToolExecutor` can append tool results with the *same* id that appears in the
/// assistant message — the API rejects mismatches.
fn register_calls(
    registry: &Mutex<HashMap<ToolCallId, (String, String)>>,
    thread_id: &str,
    calls: &[sven_tools::ToolCall],
    tools: &ToolRegistry,
) -> Vec<ProposedToolCall> {
    let mut proposed = Vec::with_capacity(calls.len());
    if let Ok(mut reg) = registry.lock() {
        for tc in calls {
            let capability = tools.capability_of(&tc.name);
            let call_id = ToolCallId::from_str_lossy(&tc.id);
            reg.insert(call_id, (thread_id.to_string(), tc.id.clone()));
            proposed.push(ProposedToolCall {
                call_id,
                name: tc.name.clone(),
                args: tc.args.clone(),
                capability,
            });
        }
    }
    proposed
}

use async_trait::async_trait;
use sven_core::{
    stream_turn, to_model_schemas, AbortedError, AgentEvent, CompactionStrategyUsed, ModelResolver,
};
use sven_hsm::{
    Effect, EffectExecutor, Event, EventSink, ObservationSink, ProposedToolCall, ToolCallId,
    UiEvent,
};
use sven_llm::{ThreadStore, TurnRequest};
use sven_model::{FunctionCall, Message, MessageContent, ResponseFormat, Role};
use sven_tools::ToolRegistry;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

/// The JSON `kind` tag that selects the single-turn engine.
pub use sven_llm::TURN_KIND;

/// Executes single-turn `CallLlm { kind: "turn" }` effects.
///
/// Owns a reference to the shared [`ThreadStore`] and the
/// `call_id → thread` registry populated for [`ToolExecutor`].
pub struct TurnExecutor {
    /// Default model used when the request does not name a per-state override.
    default_model: Arc<dyn sven_model::ModelProvider>,
    /// Optional resolver for per-state model overrides.
    model_resolver: Option<ModelResolver>,
    /// Shared tool registry; tool schemas and capabilities are resolved here.
    tools: Arc<ToolRegistry>,
    /// Append-only per-thread conversation history.
    store: Arc<Mutex<ThreadStore>>,
    /// Maps `call_id → (thread_id, original_call_id)`; fed to `ToolExecutor`
    /// so results land on the right thread using the exact id the LLM assigned.
    call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    /// Shared cancel slot; a sender stored here is dropped by the TUI abort
    /// handler to interrupt an in-flight stream.
    cancel_handle: Arc<TokioMutex<Option<oneshot::Sender<()>>>>,
    /// Consecutive-empty-turn counter per thread; see the module docs and
    /// [`EMPTY_TURN_FAILURE_THRESHOLD`]. Internal to this executor - not
    /// shared with `ToolExecutor` like `call_id_to_thread` is.
    empty_turns: Arc<Mutex<HashMap<String, u32>>>,
    /// When set (`--no-tools`), no tool schemas are ever sent to the model -
    /// the request carries only the conversation messages. Pairs with
    /// `ToolExecutor::with_no_tools` so a tool call is refused even if one
    /// somehow still arrives.
    no_tools: bool,
    /// Proactive-compaction configuration (`AgentConfig`'s `compaction_*`
    /// fields). See [`CompactionConfig`] and [`Self::with_compaction_config`].
    compaction: CompactionConfig,
    /// Running `(cache_read_total, cache_write_total)` per thread, across
    /// every turn in the session - what `UiEvent::TokenUsage.cache_read_total`/
    /// `cache_write_total` are documented to report but, before this field
    /// existed, always came back `0` (`stream_turn` only sees one turn at a
    /// time and has no memory of prior ones).
    cache_totals: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    /// Thinking-loop watchdog caps forwarded to every `stream_turn` call
    /// (main turn and compaction turn alike). See [`sven_core::ThinkingBudget`]
    /// and [`Self::with_thinking_budget`].
    thinking_budget: sven_core::ThinkingBudget,
}

/// Proactive-compaction settings, mirroring `sven_config::AgentConfig`'s
/// `compaction_*` fields (kept as a separate small struct so `TurnExecutor`'s
/// constructor signature doesn't grow every time this feature gains a knob).
#[derive(Clone)]
pub struct CompactionConfig {
    /// Fraction of the usable input budget at which compaction fires
    /// (0.0-1.0). See `AgentConfig::compaction_threshold`.
    pub threshold: f32,
    /// Fraction of the context window reserved for tool schemas, dynamic
    /// context, and estimation error - subtracted from `threshold` to get
    /// the effective trigger point. See `AgentConfig::compaction_overhead_reserve`.
    pub overhead_reserve: f32,
    /// Non-system messages preserved verbatim (not summarized) at the tail
    /// of the conversation. See `AgentConfig::compaction_keep_recent`.
    pub keep_recent: usize,
    /// Which prompt/format the model is asked to produce a summary in.
    /// See `AgentConfig::compaction_strategy`.
    pub strategy: sven_config::CompactionStrategy,
}

impl Default for CompactionConfig {
    /// Matches `sven_config::AgentConfig::default()`'s compaction fields.
    fn default() -> Self {
        Self {
            threshold: 0.85,
            overhead_reserve: 0.10,
            keep_recent: 6,
            strategy: sven_config::CompactionStrategy::Structured,
        }
    }
}

impl CompactionConfig {
    #[must_use]
    pub fn from_agent_config(cfg: &sven_config::AgentConfig) -> Self {
        Self {
            threshold: cfg.compaction_threshold,
            overhead_reserve: cfg.compaction_overhead_reserve,
            keep_recent: cfg.compaction_keep_recent,
            strategy: cfg.compaction_strategy.clone(),
        }
    }

    /// The fraction of the usable input budget at which compaction should
    /// fire, after accounting for overhead reserve. Never negative.
    fn effective_threshold(&self) -> f32 {
        (self.threshold - self.overhead_reserve).max(0.0)
    }
}

impl TurnExecutor {
    /// Create a new `TurnExecutor`.
    pub fn new(
        default_model: Arc<dyn sven_model::ModelProvider>,
        model_resolver: Option<ModelResolver>,
        tools: Arc<ToolRegistry>,
        store: Arc<Mutex<ThreadStore>>,
        call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
        cancel_handle: Arc<TokioMutex<Option<oneshot::Sender<()>>>>,
    ) -> Self {
        Self {
            default_model,
            model_resolver,
            tools,
            store,
            call_id_to_thread,
            cancel_handle,
            empty_turns: Arc::new(Mutex::new(HashMap::new())),
            no_tools: false,
            compaction: CompactionConfig::default(),
            cache_totals: Arc::new(Mutex::new(HashMap::new())),
            thinking_budget: sven_core::ThinkingBudget::default(),
        }
    }

    /// Send zero tool schemas to the model regardless of what the requesting
    /// machine asks for (`--no-tools`).
    #[must_use]
    pub fn with_no_tools(mut self, no_tools: bool) -> Self {
        self.no_tools = no_tools;
        self
    }

    /// Configure proactive compaction (see [`CompactionConfig`]).
    #[must_use]
    pub fn with_compaction_config(mut self, compaction: CompactionConfig) -> Self {
        self.compaction = compaction;
        self
    }

    /// Configure the thinking-loop watchdog caps (see
    /// [`sven_core::ThinkingBudget`]). Defaults to `ThinkingBudget::default()`
    /// (10% of the model's context window, 600s stall timeout) when not called.
    #[must_use]
    pub fn with_thinking_budget(mut self, thinking_budget: sven_core::ThinkingBudget) -> Self {
        self.thinking_budget = thinking_budget;
        self
    }

    /// Resolve the model provider, honouring a per-state override when both
    /// an override and a resolver are present.
    fn resolve_model(&self, model: &Option<String>) -> Arc<dyn sven_model::ModelProvider> {
        if let (Some(name), Some(resolver)) = (model.as_ref(), self.model_resolver.as_ref()) {
            match resolver(name) {
                Ok(m) => return m,
                Err(e) => {
                    tracing::warn!(
                        model = %name,
                        error = %e,
                        "TurnExecutor: model override failed; using default"
                    );
                }
            }
        }
        Arc::clone(&self.default_model)
    }

    /// Compact `messages` for `thread_id`, write the result back to the
    /// shared store, and return it so the in-flight turn continues with the
    /// smaller history immediately (no need to wait for the next turn).
    ///
    /// Tries LLM-based summarization first (`sven_core::prepare_compaction` /
    /// `finish_compaction`), asking the model itself for a summary of
    /// everything except the most recent `compaction.keep_recent` messages.
    /// Falls back to the deterministic, model-free `emergency_compact` (drop
    /// older history, keep a shrinking tail, no model call) whenever the
    /// LLM path can't be used or fails:
    /// - Nothing to summarize (the whole history already fits in the kept
    ///   tail) - `prepare_compaction` returns `None`.
    /// - The summarization request itself wouldn't fit the budget - asking
    ///   for a summary would just repeat the same failure one level down.
    /// - The summarization call errors, or the model returns empty text.
    ///
    /// Emits `UiEvent::ContextCompacted` on success (whichever path was
    /// used); does not emit anything and returns `messages` unchanged if
    /// there was truly nothing to compact.
    async fn compact_thread(
        &self,
        thread_id: &str,
        messages: Vec<Message>,
        model: &dyn sven_model::ModelProvider,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
        obs: &ObservationSink,
    ) -> Vec<Message> {
        let Some(plan) =
            sven_core::prepare_compaction(&messages, &self.compaction.strategy, self.compaction.keep_recent)
        else {
            return messages;
        };
        let tokens_before = plan.tokens_before;

        let budget = sven_model::budget::effective_input_budget(context_window, configured_max_output);
        let request_estimate =
            sven_model::budget::estimate_request_tokens(&plan.summarize_request, &[], None);
        let request_fits = budget.is_none_or(|b| request_estimate <= b);

        let (final_messages, strategy_used) = if request_fits {
            match self.run_compaction_turn(&plan, model, context_window, configured_max_output).await {
                Some(summary_text) if !summary_text.trim().is_empty() => {
                    let strategy_used = match self.compaction.strategy {
                        sven_config::CompactionStrategy::Structured => CompactionStrategyUsed::Structured,
                        sven_config::CompactionStrategy::Narrative => CompactionStrategyUsed::Narrative,
                    };
                    (sven_core::finish_compaction(plan, &summary_text), strategy_used)
                }
                _ => {
                    tracing::warn!(
                        thread = %thread_id,
                        "compaction: summarization call failed or returned empty text; \
                         falling back to emergency_compact"
                    );
                    let mut fallback = messages.clone();
                    sven_core::emergency_compact(&mut fallback, plan.system_msg, self.compaction.keep_recent);
                    (fallback, CompactionStrategyUsed::Emergency)
                }
            }
        } else {
            // Even the summarization request alone wouldn't fit - asking the
            // model for a summary would just fail the same way one level
            // down. Skip straight to the deterministic fallback.
            let mut fallback = messages.clone();
            sven_core::emergency_compact(&mut fallback, plan.system_msg, self.compaction.keep_recent);
            (fallback, CompactionStrategyUsed::Emergency)
        };

        let tokens_after: usize = final_messages.iter().map(Message::approx_tokens).sum();
        if let Ok(mut store) = self.store.lock() {
            store.replace_thread(thread_id, final_messages.clone());
        }
        obs.emit(UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy: strategy_used,
            turn: 0,
        });
        final_messages
    }

    /// Run the summarization call for a compaction plan and return the
    /// summary text, or `None` on any streaming/completion error.
    ///
    /// Uses a throwaway event channel: the compaction call's own text
    /// deltas must not reach the observation plane as if the agent had said
    /// them in response to the user's actual message.
    async fn run_compaction_turn(
        &self,
        plan: &sven_core::CompactionPlan,
        model: &dyn sven_model::ModelProvider,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
    ) -> Option<String> {
        let request_estimate = sven_model::budget::estimate_request_tokens(&plan.summarize_request, &[], None);
        let max_output_tokens_override =
            sven_model::budget::dynamic_output_budget(context_window, configured_max_output, request_estimate);

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let result = stream_turn(
            model,
            plan.summarize_request.clone(),
            vec![],
            None,
            None,
            None,
            max_output_tokens_override,
            self.thinking_budget,
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        match result {
            Ok((text, _tool_calls)) => Some(text),
            Err(e) => {
                tracing::warn!(error = %e, "compaction: summarization call failed");
                None
            }
        }
    }
}

#[async_trait]
impl EffectExecutor for TurnExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallLlm { request } = effect else {
            return;
        };

        if !TurnRequest::is_turn(&request) {
            tracing::warn!("TurnExecutor received a non-turn CallLlm; ignoring");
            let _ = sink
                .emit(Event::LlmFailed {
                    error: "turn executor received a non-turn request".into(),
                })
                .await;
            obs.emit(UiEvent::TurnComplete);
            return;
        }

        let req = match TurnRequest::from_value(request) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("failed to deserialise TurnRequest: {e}");
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        let thread_id = req.thread.clone();
        let model = self.resolve_model(&req.model);

        // If the request carries a user instruction, append it to the thread
        // before snapshotting so the model sees it on the very first call.
        if !req.instruction.is_empty() {
            if let Ok(mut s) = self.store.lock() {
                s.append(&thread_id, Message::user(&req.instruction));
            }
        }

        // Snapshot the thread before any async I/O (guard is dropped immediately
        // by the helper so it is never held across an await boundary).
        let mut messages = snapshot_thread(&self.store, &thread_id);

        // Resolve tool schemas: named list takes priority; fall back to all-mode.
        // `--no-tools` overrides whatever the requesting machine asked for -
        // the model never sees a single tool definition.
        let tool_schemas = if self.no_tools {
            vec![]
        } else if !req.tools.is_empty() {
            to_model_schemas(self.tools.schemas_for_names(&req.tools))
        } else if !req.all_tools_mode.is_empty() {
            let mode_val = serde_json::Value::String(req.all_tools_mode.clone());
            let mode = serde_json::from_value::<sven_config::AgentMode>(mode_val)
                .unwrap_or(sven_config::AgentMode::Agent);
            to_model_schemas(self.tools.schemas_for_mode(mode))
        } else {
            vec![]
        };

        // Observable request shape (debug level → `-v` stderr in headless
        // mode). The e2e suite asserts on `tool_schemas=`/`system_messages=`
        // to pin the --no-tools/--no-system/--bare contract - keep the field
        // names stable.
        tracing::debug!(
            thread = %thread_id,
            messages = messages.len(),
            system_messages = messages
                .iter()
                .filter(|m| matches!(m.role, Role::System))
                .count(),
            tool_schemas = tool_schemas.len(),
            "TurnExecutor: sending completion request"
        );

        // Fail fast when the request is too large for the model's known
        // effective window, instead of building it, sending it, and letting
        // the server reject it after already paying the connection/admission
        // cost (or - before the fail-loudly SSE fix - silently swallowing
        // the rejection into an empty "successful" completion). `model` here
        // is whatever `from_config_probed` resolved at session build time,
        // so `catalog_context_window()`/`catalog_max_output_tokens()` already
        // reflect the live-probed, clamped values when a probe succeeded.
        // No gate at all (not even a wrong one) when the window isn't known -
        // see `sven_model::budget::effective_input_budget`'s doc comment.
        let context_window = model.catalog_context_window();
        let configured_max_output = model.catalog_max_output_tokens();
        let mut estimate =
            sven_model::budget::estimate_request_tokens(&messages, &tool_schemas, req.dynamic_suffix.as_deref());

        // Proactive compaction: fires *before* the hard gate below, while
        // there's still enough room to ask the model for a summary. Only
        // possible when the window is known (same precondition as the gate
        // itself - see `effective_input_budget`'s doc comment).
        if let Some(budget) = sven_model::budget::effective_input_budget(context_window, configured_max_output) {
            if budget > 0 && (estimate as f32 / budget as f32) >= self.compaction.effective_threshold() {
                messages = self
                    .compact_thread(&thread_id, messages, model.as_ref(), context_window, configured_max_output, obs)
                    .await;
                estimate = sven_model::budget::estimate_request_tokens(
                    &messages,
                    &tool_schemas,
                    req.dynamic_suffix.as_deref(),
                );
            }
        }

        if let Some(budget) = sven_model::budget::effective_input_budget(context_window, configured_max_output) {
            if estimate > budget {
                let msg = format!(
                    "prompt (~{estimate} tokens) leaves no room for a response in this model's context \
                     (context {ctx} tokens, usable input budget {budget} tokens after reserving a minimal \
                     response); reduce context or raise the server's capacity",
                    ctx = context_window.unwrap_or(0),
                );
                obs.emit(UiEvent::Error(msg.clone()));
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        }
        // Scale the requested output-token limit down to whatever room this
        // specific prompt actually leaves (never up past the configured cap,
        // and never touched at all when no cap is configured or the window
        // isn't known - see the doc comment on `dynamic_output_budget`). A
        // fixed reservation of the full configured cap on every request is
        // what made a 2-token "hi" against a small-context model with a
        // capacity-sized output cap impossible to send at all.
        let max_output_tokens_override =
            sven_model::budget::dynamic_output_budget(context_window, configured_max_output, estimate);

        // Build optional structured-output constraint.
        let response_format = if req.schema.is_null() {
            None
        } else {
            Some(ResponseFormat::JsonSchema {
                name: if req.schema_name.is_empty() {
                    "decision".to_string()
                } else {
                    req.schema_name.clone()
                },
                schema: req.schema.clone(),
            })
        };

        // Bridge AgentEvents → UiEvents while the stream runs.
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let obs_fwd = obs.clone();
        // stream_turn only knows about the model call it just made - it has
        // no memory of prior turns, so it always reports cache_read_total/
        // cache_write_total as 0 and leaves max_tokens/max_output_tokens
        // unset. TurnExecutor owns the per-thread session state that's
        // actually needed to fill these in for real: running cache totals
        // (accumulated here, across every turn on this thread) and the
        // model's static capacity (already resolved above as
        // context_window/configured_max_output).
        let cache_totals = Arc::clone(&self.cache_totals);
        let totals_thread_id = thread_id.clone();
        let catalog_context_window = context_window.unwrap_or(0);
        let catalog_max_output = configured_max_output.unwrap_or(0);
        // Teed copy of the assistant text streamed so far. `stream_turn`'s
        // own return value is unreachable on cancellation (its future is
        // dropped mid-poll by the `select!` below), so this is the only
        // place the partial text survives an abort. Deliberately text-only -
        // thinking deltas are not accumulated here, since they must never be
        // fed back into the conversation store as if they were the model's
        // actual reply.
        let partial_text = Arc::new(Mutex::new(String::new()));
        let partial_text_fwd = Arc::clone(&partial_text);
        let forwarder = tokio::spawn(async move {
            while let Some(mut ev) = rx.recv().await {
                if let AgentEvent::TextDelta(delta) = &ev {
                    if let Ok(mut buf) = partial_text_fwd.lock() {
                        buf.push_str(delta);
                    }
                }
                if let AgentEvent::TokenUsage {
                    cache_read,
                    cache_write,
                    cache_read_total,
                    cache_write_total,
                    max_tokens,
                    max_output_tokens,
                    ..
                } = &mut ev
                {
                    let (read_total, write_total) = {
                        let mut totals = cache_totals
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let entry = totals.entry(totals_thread_id.clone()).or_insert((0, 0));
                        entry.0 += u64::from(*cache_read);
                        entry.1 += u64::from(*cache_write);
                        *entry
                    };
                    *cache_read_total = read_total.min(u64::from(u32::MAX)) as u32;
                    *cache_write_total = write_total.min(u64::from(u32::MAX)) as u32;
                    *max_tokens = catalog_context_window as usize;
                    *max_output_tokens = catalog_max_output as usize;
                }
                obs_fwd.emit(ev);
            }
        });

        // Install a fresh cancel channel.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *self.cancel_handle.lock().await = Some(cancel_tx);

        let result = tokio::select! {
            biased;
            _ = cancel_rx => {
                Err(anyhow::Error::new(AbortedError("turn cancelled by user".to_string())))
            }
            r = stream_turn(
                model.as_ref(),
                messages,
                tool_schemas,
                Some(thread_id.clone()),
                req.dynamic_suffix.clone(),
                response_format,
                max_output_tokens_override,
                self.thinking_budget,
                &tx,
            ) => r,
        };

        self.cancel_handle.lock().await.take();
        drop(tx);
        let _ = forwarder.await;

        // A deliberate abort (this cancel channel, or stream_turn's own
        // thinking-token/time watchdog) is reported as `UserCancelled` +
        // `Aborted`, never as `LlmFailed` + `Error` - see the "Cancellation"
        // module doc for why that distinction matters to `SdlcMachine`.
        if let Err(e) = &result {
            if let Some(aborted) = e.downcast_ref::<AbortedError>() {
                let partial = partial_text
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                let marker = format!("[aborted: {aborted}]");
                let persisted = if partial.is_empty() {
                    marker
                } else {
                    format!("{partial}\n\n{marker}")
                };
                append_messages(&self.store, &thread_id, vec![Message::assistant(&persisted)]);
                obs.emit(UiEvent::Aborted {
                    partial_text: partial,
                });
                let _ = sink.emit(Event::UserCancelled).await;
                return;
            }
        }

        let (text, tool_calls) = match result {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("{e:#}");
                obs.emit(UiEvent::Error(msg.clone()));
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        // Track consecutive empty turns (no text, no tool calls) per thread.
        // A single empty turn is tolerated - the machine's
        // `GeneratingAction::EmptyTurn` nudges the model to try again - but a
        // thread that keeps returning nothing must fail loudly rather than
        // let the run be treated as a successful (if silent) completion.
        let is_empty_turn = text.is_empty() && tool_calls.is_empty();
        let empty_turn_count = {
            let mut counts = self
                .empty_turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if is_empty_turn {
                let count = counts.entry(thread_id.clone()).or_insert(0);
                *count += 1;
                *count
            } else {
                counts.remove(&thread_id);
                0
            }
        };

        if is_empty_turn && empty_turn_count >= EMPTY_TURN_FAILURE_THRESHOLD {
            let msg = format!(
                "model returned no content and no tool calls after \
                 {empty_turn_count} consecutive attempts"
            );
            obs.emit(UiEvent::Error(msg.clone()));
            let _ = sink.emit(Event::LlmFailed { error: msg }).await;
            obs.emit(UiEvent::TurnComplete);
            return;
        }

        // Append the assistant turn to the thread (append-only, cache-safe).
        // Build the messages list first, then call the non-async helper.
        {
            let mut msgs_to_append = Vec::new();
            if !text.is_empty() {
                msgs_to_append.push(Message::assistant(&text));
            }
            for tc in &tool_calls {
                msgs_to_append.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                });
            }
            append_messages(&self.store, &thread_id, msgs_to_append);
        }

        // Annotate each tool call with its capability and register call_id → thread.
        // Uses the non-async helper to avoid holding any guard across an await.
        let proposed = register_calls(
            &self.call_id_to_thread,
            &thread_id,
            &tool_calls,
            &self.tools,
        );

        let has_tool_calls = !proposed.is_empty();
        let _ = sink
            .emit(Event::LlmTurnComplete {
                thread: thread_id,
                text,
                tool_calls: proposed,
            })
            .await;

        // Only mark the turn complete when the LLM returned tool calls or a
        // real final answer. When tool calls are pending the machine stays in
        // Generating and will trigger another LLM turn after the tools
        // finish; emitting TurnComplete here would cause headless mode to
        // exit prematurely. An under-threshold empty turn must likewise NOT
        // report TurnComplete: `GeneratingAction::EmptyTurn` needs to run its
        // nudge effect and trigger another LLM call, and headless mode
        // treats TurnComplete as "the run is over" - emitting it here would
        // exit with a false success instead of giving the nudge a chance.
        if !has_tool_calls && !is_empty_turn {
            obs.emit(UiEvent::TurnComplete);
        }
    }
}
