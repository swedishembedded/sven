// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-turn effect executor — the kernel-native turn engine.
//!
//! Handles `Effect::CallLlm { request: {"kind": "turn", ...} }` emitted by
//! any machine that uses `loop_core` (`sven-machines`).  For each turn:
//!
//! 1. Deserialises the [`TurnRequest`] from the effect payload.
//! 2. Loads a snapshot of the named thread from the shared [`ThreadStore`].
//! 3. Resolves the model provider (per-state override or default).
//! 4. Resolves tool schemas from the registry for the requested tool names.
//! 5. Calls [`sven_turn::stream_turn()`] which streams a single model response,
//!    collecting proposed tool calls (accumulation-only, no dispatch).
//! 6. Appends the assistant turn (text + tool-call messages) to the thread
//!    **append-only** (cache-safety invariant).
//! 7. Annotates each proposed tool call with its [`sven_hsm::ToolCapability`] via
//!    `registry.capability_of`.
//! 8. Registers `call_id → thread` in the shared registry so
//!    [`crate::ToolExecutor`] can append results to the right thread.
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
//! A cut-off nobody asked for (the thinking-token or thinking-time watchdog)
//! still ends the turn this way, but is preceded by a `UiEvent::Error` naming
//! the limit and the setting that raises it, so a headless run ends as a
//! failure with its reason (exit 1, outcome `agent_error`) and an embedding
//! application sees an error instead of a reply. A person's cancel emits none.
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

use async_trait::async_trait;
use sven_decision::telemetry::DecisionTelemetry;
use sven_hsm::{Effect, Event, ObservationSink, ToolCallId, UiEvent};
use sven_kernel::{EffectExecutor, EventSink};
use sven_model::{FunctionCall, Message, MessageContent, ResponseFormat, Role};
use sven_tool_registry::ToolRegistry;
use sven_turn::{to_model_schemas, AbortedError, ModelResolver};
use sven_vocab::TurnRequest;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

mod compaction;
mod context_view;
mod cost_gate;

pub use cost_gate::CostPolicy;
mod instructions;
mod limits;
mod overflow;
mod replies;

use limits::record_estimate_scale;
pub use limits::CompactionConfig;

use crate::chunks::ContextAssembler;
use crate::notices;
use crate::thread_store::{
    append_messages, machine_answers, register_calls, snapshot_thread, ThreadStore,
};

/// Executes single-turn `CallLlm { kind: "turn" }` effects.
///
/// Owns a reference to the shared [`ThreadStore`] and the
/// `call_id → thread` registry populated for [`crate::ToolExecutor`].
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
    /// Limits forwarded to every `stream_turn` call (main turn and
    /// compaction turn alike). See [`Self::with_turn_limits`].
    limits: sven_turn::TurnLimits,
    /// Models already warned about for lacking tool calling, so each is
    /// mentioned once per session however many turns it serves.
    warned_without_tools: std::collections::HashSet<String>,
    /// Decision accounting charged to the next request's ledger record.
    decisions: Option<Arc<DecisionTelemetry>>,
    /// Builds the context view of each request when addressable context is on.
    context: Option<ContextAssembler>,
    /// How a demotion is weighed against the provider's prompt cache.
    cost: CostPolicy,
    /// What the previous requests left in that cache.
    cache_usage: Arc<Mutex<cost_gate::CacheUsage>>,
    /// How many tokens the server counted per token this executor estimated
    /// for the last request that reported usage (1.0 until one has). The
    /// chars/4 estimate undercounts code by tens of percent, so the
    /// compaction trigger scales its estimate by this to act on the server's
    /// count instead of waiting for the server to reject an oversized request.
    estimate_scale: Arc<Mutex<f32>>,
    /// The gate's view of the request being built.
    cache_ctx: Option<crate::CacheContext>,
    /// The rebuild decisions made for the request being built, for its record.
    rebuilds: Vec<sven_vocab::cost::RebuildRecord>,
    /// Activates and announces conditional instructions each turn.
    instructions: Option<Arc<sven_turn::instructions::Activator>>,
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
            warned_without_tools: std::collections::HashSet::new(),
            decisions: None,
            context: None,
            cost: CostPolicy::default(),
            cache_usage: Arc::new(Mutex::new(cost_gate::CacheUsage::default())),
            estimate_scale: Arc::new(Mutex::new(1.0)),
            cache_ctx: None,
            rebuilds: Vec::new(),
            instructions: None,
            no_tools: false,
            compaction: CompactionConfig::default(),
            cache_totals: Arc::new(Mutex::new(HashMap::new())),
            limits: sven_turn::TurnLimits::default(),
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

    /// Configure the limits on each streamed turn (see
    /// [`sven_turn::TurnLimits`]). Defaults to `TurnLimits::default()` (10% of
    /// the model's context window for thinking, 600s thinking stall, 300s
    /// stream idle) when not called.
    #[must_use]
    pub fn with_turn_limits(mut self, limits: sven_turn::TurnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Charges what `telemetry` gathered to the next request's ledger record.
    #[must_use]
    pub fn with_decision_telemetry(mut self, telemetry: Arc<DecisionTelemetry>) -> Self {
        self.decisions = Some(telemetry);
        self
    }

    /// Builds each request's context from the history with `assembler`
    /// instead of sending the history as it is. `None` sends the history.
    #[must_use]
    pub fn with_context_assembler(mut self, assembler: Option<ContextAssembler>) -> Self {
        self.context = assembler;
        self
    }

    /// Announces the instructions whose activation holds with each request and
    /// records newly activated ones in the kernel. `None` announces nothing.
    #[must_use]
    pub fn with_instructions(
        mut self,
        instructions: Option<Arc<sven_turn::instructions::Activator>>,
    ) -> Self {
        self.instructions = instructions;
        self
    }

    /// The provider for the turn: the host's provider for its `role` (an
    /// error when there is none), else as [`Self::resolve_model`].
    fn resolve_role(
        &self,
        req: &TurnRequest,
    ) -> Result<Arc<dyn sven_model::ModelProvider>, String> {
        let Some(role) = &req.role else {
            return Ok(self.resolve_model(&req.model));
        };
        let resolver = self
            .model_resolver
            .as_ref()
            .ok_or_else(|| format!("model role `{role}` is not provided by the host"))?;
        resolver(role).map_err(|e| format!("model role `{role}` is not provided by the host: {e}"))
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

        let mut req = match TurnRequest::from_value(request) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("failed to deserialise TurnRequest: {e}");
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        let thread_id = req.thread.clone();
        // A turn a harness asked for is judged by the harness: an empty reply
        // is its to retry or to name, not the executor's to count as a streak
        // across the checks and repairs between turns.
        let judged_by_harness = req.origin.is_some();
        if let Some(ms) = req.delay_ms {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        let model = match self.resolve_role(&req) {
            Ok(model) => model,
            Err(error) => {
                obs.emit(UiEvent::Error(error.clone()));
                let _ = sink.emit(Event::LlmFailed { error }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        // Answer the calls the machine answered itself, then the instruction: a valid history.
        let answers = machine_answers(&self.call_id_to_thread, &req.answered_calls);
        if let Ok(mut s) = self.store.lock() {
            s.answer_open_calls(&thread_id, &answers);
            s.open_thread(&thread_id, req.system.as_deref());
            if !req.instruction.is_empty() {
                match &req.origin {
                    Some(origin) => {
                        s.append_labeled(&thread_id, Message::user(&req.instruction), origin);
                    }
                    None => s.append(&thread_id, Message::user(&req.instruction)),
                }
            }
        }

        // Snapshot the thread before any async I/O (guard is dropped immediately
        // by the helper so it is never held across an await boundary).
        let mut messages = snapshot_thread(&self.store, &thread_id);
        instructions::announce(self.instructions.as_deref(), &mut req, &messages, sink).await;

        // Resolve tool schemas: named list takes priority; fall back to all-mode.
        // `--no-tools` overrides whatever the requesting machine asked for -
        // the model never sees a single tool definition. The context view is
        // decided with them: a request that carries a reference offers the tool
        // that expands it (see `context_view`).
        self.cache_ctx = self.cache_context(model.as_ref());
        self.rebuilds.clear();
        let planned = self.plan_request(&req, &messages);
        let expandable = context_view::offers_expansion(&planned.offered);
        let mut view = planned.view;
        let withheld_tools = planned.offered.withheld;
        let tool_schemas = to_model_schemas(planned.offered.schemas);
        if !planned.activate.is_empty() {
            let _ = sink
                .emit(Event::ToolsActivated {
                    names: planned.activate,
                })
                .await;
        }

        notices::report_served_model_change(model.as_ref(), obs);
        notices::warn_if_tools_unsupported(
            &mut self.warned_without_tools,
            model.as_ref(),
            !tool_schemas.is_empty(),
            obs,
        );

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
        let limits = self.limits.with_turn_decoding(&req.decoding);
        let configured_max_output = limits.cap_output(model.catalog_max_output_tokens());
        let mut estimate = sven_model::budget::estimate_request_tokens(
            view.as_ref().map_or(&messages, |v| &v.0),
            &tool_schemas,
            req.dynamic_suffix.as_deref(),
        );

        // Proactive compaction: fires *before* the hard gate below, while
        // there's still enough room to ask the model for a summary. Only
        // possible when the window is known (same precondition as the gate
        // itself - see `effective_input_budget`'s doc comment).
        if let Some(budget) =
            sven_model::budget::effective_input_budget(context_window, configured_max_output)
        {
            let scale = *self
                .estimate_scale
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if budget > 0
                && (estimate as f32 * scale / budget as f32)
                    >= self.compaction.effective_threshold()
            {
                messages = self
                    .compact_thread(
                        &thread_id,
                        messages,
                        model.as_ref(),
                        context_window,
                        configured_max_output,
                        obs,
                    )
                    .await;
                view = self.reassemble_view(&messages, expandable);
                estimate = sven_model::budget::estimate_request_tokens(
                    view.as_ref().map_or(&messages, |v| &v.0),
                    &tool_schemas,
                    req.dynamic_suffix.as_deref(),
                );
            }
        }
        let (messages, context_view) = match view {
            Some((assembled, record)) => {
                self.mark_view_sent(&record);
                (assembled, Some(record))
            }
            None => (messages, None),
        };

        if let Some(budget) =
            sven_model::budget::effective_input_budget(context_window, configured_max_output)
        {
            if estimate > budget {
                let msg = format!(
                    "prompt (~{estimate} tokens) leaves no room for a response in this model's context \
                     (context {ctx} tokens, usable input budget {budget} tokens after reserving a minimal \
                     response); reduce context or raise the server's capacity \
                     [provider={provider} model={model}]",
                    ctx = context_window.unwrap_or(0),
                    provider = model.name(),
                    model = model.model_name(),
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
            limits.output_budget(context_window, configured_max_output, estimate);

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

        // Forward stream_turn's progress events onward while the stream runs.
        let (tx, mut rx) = mpsc::channel::<UiEvent>(256);
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
        let decisions = self.decisions.clone();
        let cache_usage = Arc::clone(&self.cache_usage);
        let estimate_scale = Arc::clone(&self.estimate_scale);
        let estimated_prompt_tokens = estimate;
        let rebuilds = std::mem::take(&mut self.rebuilds);
        // Teed copy of the assistant text streamed so far. `stream_turn`'s
        // own return value is unreachable on cancellation (its future is
        // dropped mid-poll by the `select!` below), so this is the only
        // place the partial text survives an abort. Deliberately text-only -
        // thinking deltas are not accumulated here, since they must never be
        // fed back into the conversation store as if they were the model's
        // actual reply.
        let partial_text = Arc::new(Mutex::new(String::new()));
        let partial_text_fwd = Arc::clone(&partial_text);
        // The store keeps each request's account beside the conversation it
        // belongs to, so a run's record survives a lagging observation stream.
        let request_store = Arc::clone(&self.store);
        let request_thread = thread_id.clone();
        let forwarder = tokio::spawn(async move {
            let mut requests = sven_vocab::RequestFold::default();
            while let Some(mut ev) = rx.recv().await {
                if let UiEvent::TextDelta(delta) = &ev {
                    if let Ok(mut buf) = partial_text_fwd.lock() {
                        buf.push_str(delta);
                    }
                }
                if let UiEvent::TokenUsage {
                    cache_read,
                    cache_write,
                    cache_read_total,
                    cache_write_total,
                    max_tokens,
                    max_output_tokens,
                    breakdown,
                    input,
                    ..
                } = &mut ev
                {
                    record_estimate_scale(
                        &estimate_scale,
                        estimated_prompt_tokens,
                        u64::from(*input) + u64::from(*cache_read) + u64::from(*cache_write),
                    );
                    if let Some(breakdown) = breakdown {
                        if let Some(decisions) = &decisions {
                            notices::charge_decisions(decisions, breakdown);
                        }
                        breakdown.context_view.clone_from(&context_view);
                        if !rebuilds.is_empty() {
                            breakdown.cost = Some(sven_vocab::cost::CostRecord {
                                rebuilds: rebuilds.clone(),
                            });
                        }
                        // What this request read and wrote is what the cache
                        // is expected to hold for the next.
                        cache_usage
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .finished(cache_read.saturating_add(*cache_write));
                    }
                    (*cache_read_total, *cache_write_total) = notices::add_cache_totals(
                        &cache_totals,
                        &totals_thread_id,
                        (*cache_read, *cache_write),
                    );
                    *max_tokens = catalog_context_window as usize;
                    *max_output_tokens = catalog_max_output as usize;
                }
                if let Some(request) = requests.observe(&ev) {
                    if let Ok(mut store) = request_store.lock() {
                        store.record_request(&request_thread, request);
                    }
                }
                obs_fwd.emit(ev);
            }
        });

        // Install a fresh cancel channel.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *self.cancel_handle.lock().await = Some(cancel_tx);

        let todos = self.pinned_todos();
        let result = tokio::select! {
            biased;
            _ = cancel_rx => {
                // A caller that stops the run from outside says why; a
                // person's Ctrl+C does not.
                let reason = self
                    .store
                    .lock()
                    .ok()
                    .and_then(|mut store| store.take_stop_reason())
                    .unwrap_or_else(|| "turn cancelled by user".to_string());
                Err(anyhow::Error::new(AbortedError::by_user(reason)))
            }
            () = turn_deadline(limits.deadline) => {
                let secs = limits.deadline.map_or(0, |d| d.as_secs());
                Err(anyhow::Error::new(AbortedError::by_watchdog(format!(
                    "the turn reached its deadline of {secs} s"
                ))))
            }
            r = overflow::Request {
                messages,
                tools: tool_schemas,
                cache_key: Some(thread_id.clone()),
                dynamic_suffix: req.dynamic_suffix.clone(),
                response_format,
                max_output_tokens_override,
            }
            .stream_retrying_overflow(
                model.as_ref(),
                limits,
                self.compaction.keep_recent,
                &todos,
                &tx,
                obs,
            ) => r,
        };

        self.cancel_handle.lock().await.take();
        drop(tx);
        let _ = forwarder.await;
        notices::report_served_model_change(model.as_ref(), obs);

        // A deliberate abort (this cancel channel, or stream_turn's own
        // thinking-token/time watchdog) is reported as `UserCancelled` +
        // `Aborted`, never as `LlmFailed` + `Error` - see the "Cancellation"
        // module doc for why that distinction matters to `SdlcMachine`.
        if let Err(e) = &result {
            if let Some(aborted) = e.downcast_ref::<AbortedError>() {
                let partial = partial_text.lock().map(|g| g.clone()).unwrap_or_default();
                let marker = format!("[aborted: {aborted}]");
                // The abort still ends the turn as a cancel (see the module
                // docs), but a cut-off nobody asked for says why, as an error,
                // before it: a headless run then ends as a failure with its
                // reason instead of as a silent success.
                if !aborted.by_user {
                    obs.emit(UiEvent::Error(aborted.to_string()));
                }
                let persisted = if partial.is_empty() {
                    marker
                } else {
                    format!("{partial}\n\n{marker}")
                };
                append_messages(
                    &self.store,
                    &thread_id,
                    vec![Message::assistant(&persisted)],
                );
                obs.emit(UiEvent::Aborted {
                    partial_text: partial,
                });
                let _ = sink.emit(Event::UserCancelled).await;
                return;
            }
        }

        let (text, tool_calls, unparsed, truncated) = match result {
            Ok(turn) => {
                let unparsed = turn.unparsed_arguments.clone();
                if let Ok(mut store) = self.store.lock() {
                    store.record_unparsed_arguments(turn.unparsed_arguments);
                }
                (turn.text, turn.tool_calls, unparsed, turn.truncated)
            }
            Err(e) => {
                let msg = replies::failure_text(&format!("{e:#}"));
                obs.emit(UiEvent::Error(msg.clone()));
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        // Track consecutive empty turns (no tool calls, and no text beyond
        // whitespace) per thread. One is tolerated - the machine's
        // `GeneratingAction::EmptyTurn` nudges the model to try again - but a
        // thread that keeps returning nothing must fail loudly, not end as a
        // silent success.
        let is_empty_turn = text.trim().is_empty() && tool_calls.is_empty();
        let empty_turn_count = {
            let mut counts = self
                .empty_turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if is_empty_turn && judged_by_harness {
                0
            } else if is_empty_turn {
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

        // A call to a tool the model was never given the definition of is
        // still proposed, gated and audited like any other; the tool executor
        // then answers it with an instruction to load the tool.
        for call in proposed.iter().filter(|c| withheld_tools.contains(&c.name)) {
            self.tools.hold_back(call.call_id);
        }

        // A call whose arguments the model garbled beyond repair carries `{}`;
        // it is refused with the reason rather than run on those arguments.
        for (tc, call) in tool_calls.iter().zip(&proposed) {
            if let Some(raw) = unparsed.get(&tc.id) {
                self.tools.refuse_unparsed(call.call_id, &tc.name, raw);
            }
        }

        let has_tool_calls = !proposed.is_empty();
        if truncated {
            replies::tell_truncated(sink).await;
        }
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

/// Resolves when a turn's wall-clock bound passes; never when it has none.
async fn turn_deadline(deadline: Option<std::time::Duration>) {
    match deadline {
        Some(limit) => tokio::time::sleep(limit).await,
        None => std::future::pending().await,
    }
}
