//! Converse effect executor — the reactive agent's turn engine.
//!
//! Handles the `Effect::CallLlm { request: {"kind": "converse", "text": ...} }`
//! effect emitted by
//! [`sven_core::ReactiveAgentMachine`](sven_core::ReactiveAgentMachine).
//!
//! It owns a shared [`Agent`] and reproduces the *exact* legacy agentic-loop
//! fidelity (streaming text/thinking deltas, native parallel tool calls,
//! XML/Hermes fallback, empty-turn retries, `max_tool_rounds` wrap-up, mid-loop
//! compaction, model/mode switching) by delegating to
//! [`Agent::submit_with_cancel`]. Every [`AgentEvent`] the loop produces is
//! bridged onto the outward [`ObservationSink`] as a [`UiEvent`] while the
//! turn is in flight; when the loop settles, exactly one inward completion
//! [`Event`] (`LlmProposedResponse` on success, `LlmFailed` on error) is
//! posted so the machine can advance, followed by `UiEvent::TurnComplete`
//! on the outward plane so the TUI can update its busy state.
//!
//! # TurnComplete ordering guarantee
//!
//! `UiEvent::TurnComplete` is emitted **after** the inward completion event
//! (`LlmProposedResponse` / `LlmFailed`) is already in the kernel queue.
//! This prevents the TUI from marking the turn complete and dequeuing the
//! next message before the machine has transitioned back to `Idle`, which
//! would cause the second message to be silently dropped.
//!
//! # Cancellation
//!
//! `ConverseExecutor` holds a shared cancel slot
//! (`Arc<Mutex<Option<oneshot::Sender<()>>>>`). Before each LLM submission
//! it creates a fresh oneshot pair, stores the sender in the slot, and
//! passes the receiver to [`Agent::submit_with_cancel`]. The TUI's
//! `/abort` command drops the sender from the slot, which cancels the
//! in-flight LLM call.

use std::sync::Arc;

use async_trait::async_trait;
use sven_core::{Agent, AgentEvent};
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink, UiEvent};
use tokio::sync::{mpsc, oneshot, Mutex};

/// The JSON `kind` tag that selects the converse turn engine.
pub const CONVERSE_KIND: &str = "converse";

/// Executes converse turns by driving a shared [`Agent`].
pub struct ConverseExecutor {
    agent: Arc<Mutex<Agent>>,
    /// Shared cancel slot. Before each submission the executor stores the
    /// sender half of a fresh oneshot here; the TUI's abort handler drops
    /// it to cancel the in-flight call.
    cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ConverseExecutor {
    /// Wraps a shared agent. The same `Arc` may be held elsewhere (e.g. to
    /// inspect the session), but only one converse turn runs at a time because
    /// `execute` holds the lock for the whole turn (respecting RTC).
    ///
    /// `cancel_handle` is the shared slot the TUI uses to cancel an in-flight
    /// turn. Pass the same `Arc` that `App::agent.cancel` points to so that
    /// the TUI's `/abort` command reaches this executor directly.
    pub fn new(
        agent: Arc<Mutex<Agent>>,
        cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    ) -> Self {
        Self {
            agent,
            cancel_handle,
        }
    }
}

#[async_trait]
impl EffectExecutor for ConverseExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallLlm { request } = effect else {
            return;
        };

        // Only handle converse requests; anything else is a wiring error.
        if request.get("kind").and_then(|v| v.as_str()) != Some(CONVERSE_KIND) {
            tracing::warn!("ConverseExecutor received a non-converse CallLlm request; ignoring");
            let _ = sink
                .emit(Event::LlmFailed {
                    error: "converse executor received a non-converse request".into(),
                })
                .await;
            obs.emit(UiEvent::TurnComplete);
            return;
        }

        let text = request
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        tracing::info!(text_len = text.len(), "ConverseExecutor: starting LLM turn");

        // Bridge AgentEvents → UiEvents while the loop runs, and capture the
        // final assistant text for the inward completion event.
        // NOTE: TurnComplete is filtered here and emitted below, AFTER the
        // kernel completion event, to prevent the race where the TUI dequeues
        // the next message before the machine transitions back to Idle.
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let obs_fwd = obs.clone();
        let forwarder = tokio::spawn(async move {
            let mut final_text = String::new();
            let mut event_count = 0u32;
            while let Some(ev) = rx.recv().await {
                event_count += 1;
                tracing::debug!(event_count, kind = ?std::mem::discriminant(&ev), "ConverseExecutor forwarder: got event");
                if let AgentEvent::TextComplete(t) = &ev {
                    if !t.is_empty() {
                        final_text = t.clone();
                    }
                }
                if let Some(ui) = agent_event_to_ui(ev) {
                    obs_fwd.emit(ui);
                }
            }
            tracing::info!(event_count, final_text_len = final_text.len(), "ConverseExecutor forwarder: done");
            final_text
        });

        // Install a fresh cancel channel so the TUI can abort this turn.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *self.cancel_handle.lock().await = Some(cancel_tx);

        tracing::info!("ConverseExecutor: calling submit_with_cancel");
        let result = {
            let mut agent = self.agent.lock().await;
            agent.submit_with_cancel(&text, tx, cancel_rx).await
        };
        tracing::info!(ok = result.is_ok(), "ConverseExecutor: submit_with_cancel returned");

        // Clear the cancel slot so a stale abort doesn't leak into the next turn.
        self.cancel_handle.lock().await.take();

        let final_text = forwarder.await.unwrap_or_default();
        tracing::info!(final_text_len = final_text.len(), "ConverseExecutor: forwarder joined");

        // Post the inward completion event FIRST, then emit TurnComplete outward.
        // The kernel consumer processes LlmProposedResponse / LlmFailed and
        // transitions the machine to Idle before the TUI sees TurnComplete and
        // potentially dequeues the next user message.
        match result {
            Ok(()) => {
                tracing::info!("ConverseExecutor: emitting LlmProposedResponse");
                let _ = sink
                    .emit(Event::LlmProposedResponse { text: final_text })
                    .await;
            }
            Err(e) => {
                let error_msg = format!("{e:#}");
                tracing::warn!(error = %error_msg, "ConverseExecutor: emitting LlmFailed");
                let _ = sink
                    .emit(Event::LlmFailed {
                        error: error_msg.clone(),
                    })
                    .await;
                // Forward the error to the TUI so it is rendered as a visible
                // error segment.  Without this the user sees nothing - the
                // machine silently transitions back to Idle and TurnComplete
                // fires but no response or error is displayed.
                obs.emit(UiEvent::Error(error_msg));
            }
        }

        tracing::info!("ConverseExecutor: emitting TurnComplete");
        obs.emit(UiEvent::TurnComplete);
    }
}

/// Maps a legacy [`AgentEvent`] to the equivalent outward [`UiEvent`].
///
/// Returns `None` for events that have no renderable observation counterpart
/// (e.g. internal question/answer plumbing handled by other channels).
///
/// `TurnComplete` is intentionally excluded here and emitted directly by
/// [`ConverseExecutor::execute`] after the inward completion event is in the
/// kernel queue, preventing the TUI from dequeuing the next message prematurely.
fn agent_event_to_ui(ev: AgentEvent) -> Option<UiEvent> {
    Some(match ev {
        AgentEvent::TextDelta(d) => UiEvent::TextDelta(d),
        AgentEvent::TextComplete(t) => UiEvent::TextComplete(t),
        AgentEvent::ThinkingDelta(d) => UiEvent::ThinkingDelta(d),
        AgentEvent::ThinkingComplete(c) => UiEvent::ThinkingComplete(c),
        AgentEvent::ToolCallStarted(tc) => UiEvent::ToolStarted {
            call_id: tc.id,
            name: tc.name,
            args: tc.args,
        },
        AgentEvent::ToolCallFinished {
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
        AgentEvent::ToolProgress { call_id, message } => UiEvent::ToolProgress { call_id, message },
        AgentEvent::ContextCompacted {
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
        AgentEvent::TokenUsage {
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
        AgentEvent::TurnComplete => return None,
        AgentEvent::Aborted { partial_text } => UiEvent::Aborted { partial_text },
        AgentEvent::Error(e) => UiEvent::Error(e),
        AgentEvent::TodoUpdate(items) => {
            UiEvent::TodoUpdate(serde_json::to_value(&items).unwrap_or(serde_json::Value::Null))
        }
        AgentEvent::ModeChanged(mode) => UiEvent::ModeChanged(format!("{mode:?}")),
        AgentEvent::ModelChanged(m) => UiEvent::ModelChanged(m),
        // No renderable observation equivalent (yet) for these.
        AgentEvent::Question { .. }
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::TitleGenerated(_)
        | AgentEvent::CollabEvent(_)
        | AgentEvent::DelegateSummary { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentEvent { .. }
        | AgentEvent::PeerList(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use sven_core::{AgentNewParams, AgentRuntimeContext};
    use sven_hsm::{Context, Hsm, Machine, MachineId, PermissionPolicy, Reaction, Runtime};
    use sven_model::{CompletionRequest, ModelProvider, ResponseEvent};
    use sven_tools::ToolRegistry;

    /// Streams a fixed reply then `Done`.
    struct PongProvider;

    #[async_trait]
    impl ModelProvider for PongProvider {
        fn name(&self) -> &str {
            "pong"
        }
        fn model_name(&self) -> &str {
            "pong"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let events: Vec<anyhow::Result<ResponseEvent>> = vec![
                Ok(ResponseEvent::TextDelta("pong".into())),
                Ok(ResponseEvent::Done),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn build_agent() -> Agent {
        let (_tx, rx) = mpsc::channel(8);
        Agent::new_with_params(AgentNewParams {
            model: Arc::new(PongProvider),
            tools: Arc::new(ToolRegistry::new()),
            config: Arc::new(sven_config::AgentConfig::default()),
            runtime: AgentRuntimeContext::default(),
            mode_lock: Arc::new(Mutex::new(sven_config::AgentMode::Agent)),
            tool_event_rx: rx,
            max_context_tokens: 128_000,
            model_resolver: None,
        })
    }

    // Minimal machine that records the kind of the first domain event it sees.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct OneShot(MachineId);
    impl Machine for OneShot {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, _s: TS) -> TS {
            TS::Top
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Idle if !e.is_lifecycle() => {
                    ctx.set_fact("kind", format!("{:?}", e.kind()));
                    if let Event::LlmProposedResponse { text } = e {
                        ctx.set_fact("text", serde_json::json!(text));
                    }
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
                    }
                }
                _ => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOp;
    #[async_trait]
    impl EffectExecutor for NoOp {
        async fn execute(&mut self, _e: Effect, _s: &EventSink, _o: &ObservationSink) {}
    }

    fn make_executor() -> ConverseExecutor {
        let agent = Arc::new(Mutex::new(build_agent()));
        let cancel_handle = Arc::new(Mutex::new(None));
        ConverseExecutor::new(agent, cancel_handle)
    }

    #[tokio::test]
    async fn converse_streams_text_and_posts_response() {
        let mut exec = make_executor();

        let rt = Runtime::spawn(
            Hsm::new(OneShot(MachineId::new())),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOp,
            16,
        );
        let sink = rt.sink();
        let obs = ObservationSink::new(64);
        let mut obs_rx = obs.subscribe();

        let effect = Effect::CallLlm {
            request: serde_json::json!({ "kind": CONVERSE_KIND, "text": "ping" }),
        };
        exec.execute(effect, &sink, &obs).await;

        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("LlmProposedResponse")
        );
        assert_eq!(
            report.ctx.fact("text").and_then(|v| v.as_str()),
            Some("pong")
        );

        // The streamed text delta reached the outward observation plane.
        let mut saw_delta = false;
        let mut saw_turn_complete = false;
        while let Ok(ev) = obs_rx.try_recv() {
            if ev == UiEvent::TextDelta("pong".into()) {
                saw_delta = true;
            }
            if ev == UiEvent::TurnComplete {
                saw_turn_complete = true;
            }
        }
        assert!(saw_delta, "expected a TextDelta('pong') observation");
        assert!(
            saw_turn_complete,
            "expected TurnComplete on the observation plane"
        );
    }

    #[tokio::test]
    async fn non_converse_request_fails_gracefully() {
        let mut exec = make_executor();

        let rt = Runtime::spawn(
            Hsm::new(OneShot(MachineId::new())),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOp,
            16,
        );
        let sink = rt.sink();
        let obs = ObservationSink::new(8);

        let effect = Effect::CallLlm {
            request: serde_json::json!({ "kind": "extract_intent", "text": "x" }),
        };
        exec.execute(effect, &sink, &obs).await;

        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("LlmFailed")
        );
    }
}
