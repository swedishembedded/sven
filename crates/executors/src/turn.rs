// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-turn effect executor — the kernel-native turn engine.
//!
//! Handles `Effect::CallLlm { request: {"kind": "turn", ...} }` emitted by
//! any machine that uses [`crate::loop_core`].  For each turn:
//!
//! 1. Deserialises the [`TurnRequest`] from the effect payload.
//! 2. Loads a snapshot of the named thread from the shared [`ConversationStore`].
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
//! A shared cancel slot allows the TUI to abort an in-flight turn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ─── Sync helpers ─────────────────────────────────────────────────────────────

/// Snapshot a thread from the store (non-async; guard is never held across await).
fn snapshot_thread(store: &Mutex<ConversationStore>, thread_id: &str) -> Vec<Message> {
    store
        .lock()
        .map(|s| s.snapshot(thread_id))
        .unwrap_or_default()
}

/// Append a batch of messages to a store thread (non-async).
fn append_messages(store: &Mutex<ConversationStore>, thread_id: &str, messages: Vec<Message>) {
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
use sven_core::{stream_turn, to_model_schemas, AgentEvent, ModelResolver};
use sven_hsm::{
    Effect, EffectExecutor, Event, EventSink, ObservationSink, ProposedToolCall, ToolCallId,
    UiEvent,
};
use sven_llm::{ConversationStore, TurnRequest};
use sven_model::{FunctionCall, Message, MessageContent, ResponseFormat, Role};
use sven_tools::ToolRegistry;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

/// Convert an [`AgentEvent`] to a [`UiEvent`] for the outward observation plane.
///
/// Returns `None` for events that have no renderable UI equivalent or that are
/// handled directly by the executor (`TurnComplete`).
pub fn agent_event_to_ui(ev: AgentEvent) -> Option<UiEvent> {
    use sven_core::AgentEvent as AE;
    Some(match ev {
        AE::TextDelta(d) => UiEvent::TextDelta(d),
        AE::TextComplete(t) => UiEvent::TextComplete(t),
        AE::ThinkingDelta(d) => UiEvent::ThinkingDelta(d),
        AE::ThinkingComplete(c) => UiEvent::ThinkingComplete(c),
        AE::ToolCallStarted(tc) => UiEvent::ToolStarted {
            call_id: tc.id,
            name: tc.name,
            args: tc.args,
        },
        AE::ToolCallFinished {
            call_id,
            tool_name,
            output,
            is_error,
        } => UiEvent::ToolFinished {
            call_id,
            name: tool_name,
            output,
            is_error,
        },
        AE::ToolProgress { call_id, message } => UiEvent::ToolProgress { call_id, message },
        AE::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy: strategy.to_string(),
            turn,
        },
        AE::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            cost_usd,
        } => UiEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            cost_usd,
        },
        // TurnComplete is handled by execute() directly (ordering guarantee).
        AE::TurnComplete => return None,
        AE::Aborted { partial_text } => UiEvent::Aborted { partial_text },
        AE::Error(e) => UiEvent::Error(e),
        AE::TodoUpdate(items) => {
            UiEvent::TodoUpdate(serde_json::to_value(&items).unwrap_or(serde_json::Value::Null))
        }
        AE::ModeChanged(mode) => UiEvent::ModeChanged(format!("{mode:?}")),
        AE::ModelChanged(m) => UiEvent::ModelChanged(m),
        // Subagent / delegate / team observations pass through the outward
        // plane so the frontend can render child-session views, delegate
        // summaries, and collab segments. Complex payloads are carried as
        // opaque JSON so the kernel stays dependency-free.
        AE::SubagentStarted {
            call_id,
            handle_id,
            description,
            prompt,
        } => UiEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            prompt,
        },
        AE::SubagentEvent {
            call_id,
            handle_id,
            update,
        } => UiEvent::SubagentEvent {
            call_id,
            handle_id,
            update: serde_json::to_value(&update).unwrap_or(serde_json::Value::Null),
        },
        AE::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        } => UiEvent::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        },
        AE::CollabEvent(e) => {
            UiEvent::CollabEvent(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null))
        }
        AE::PeerList(peers) => {
            UiEvent::PeerList(serde_json::to_value(&peers).unwrap_or(serde_json::Value::Null))
        }
        // No renderable observation equivalent for these.
        AE::Question { .. } | AE::QuestionAnswer { .. } | AE::TitleGenerated(_) => return None,
    })
}

/// The JSON `kind` tag that selects the single-turn engine.
pub use sven_llm::TURN_KIND;

/// Executes single-turn `CallLlm { kind: "turn" }` effects.
///
/// Owns a reference to the shared [`ConversationStore`] and the
/// `call_id → thread` registry populated for [`ToolExecutor`].
pub struct TurnExecutor {
    /// Default model used when the request does not name a per-state override.
    default_model: Arc<dyn sven_model::ModelProvider>,
    /// Optional resolver for per-state model overrides.
    model_resolver: Option<ModelResolver>,
    /// Shared tool registry; tool schemas and capabilities are resolved here.
    tools: Arc<ToolRegistry>,
    /// Append-only per-thread conversation history.
    store: Arc<Mutex<ConversationStore>>,
    /// Maps `call_id → (thread_id, original_call_id)`; fed to `ToolExecutor`
    /// so results land on the right thread using the exact id the LLM assigned.
    call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    /// Shared cancel slot; a sender stored here is dropped by the TUI abort
    /// handler to interrupt an in-flight stream.
    cancel_handle: Arc<TokioMutex<Option<oneshot::Sender<()>>>>,
}

impl TurnExecutor {
    /// Create a new `TurnExecutor`.
    pub fn new(
        default_model: Arc<dyn sven_model::ModelProvider>,
        model_resolver: Option<ModelResolver>,
        tools: Arc<ToolRegistry>,
        store: Arc<Mutex<ConversationStore>>,
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
        }
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
        let messages = snapshot_thread(&self.store, &thread_id);

        // Resolve tool schemas: named list takes priority; fall back to all-mode.
        let tool_schemas = if !req.tools.is_empty() {
            to_model_schemas(self.tools.schemas_for_names(&req.tools))
        } else if !req.all_tools_mode.is_empty() {
            let mode_val = serde_json::Value::String(req.all_tools_mode.clone());
            let mode = serde_json::from_value::<sven_config::AgentMode>(mode_val)
                .unwrap_or(sven_config::AgentMode::Agent);
            to_model_schemas(self.tools.schemas_for_mode(mode))
        } else {
            vec![]
        };

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
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if let Some(ui) = agent_event_to_ui(ev) {
                    obs_fwd.emit(ui);
                }
            }
        });

        // Install a fresh cancel channel.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *self.cancel_handle.lock().await = Some(cancel_tx);

        let result = tokio::select! {
            biased;
            _ = cancel_rx => {
                Err(anyhow::anyhow!("turn cancelled"))
            }
            r = stream_turn(
                model.as_ref(),
                messages,
                tool_schemas,
                Some(thread_id.clone()),
                req.dynamic_suffix.clone(),
                response_format,
                &tx,
            ) => r,
        };

        self.cancel_handle.lock().await.take();
        drop(tx);
        let _ = forwarder.await;

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

        // Only mark the turn complete when the LLM returned no tool calls.
        // When tool calls are pending the machine stays in Generating and will
        // trigger another LLM turn after the tools finish; emitting TurnComplete
        // here would cause headless mode to exit prematurely.
        if !has_tool_calls {
            obs.emit(UiEvent::TurnComplete);
        }
    }
}
