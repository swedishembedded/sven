//! Deliberation effect executor — the SDLC state engine.
//!
//! Handles `Effect::CallLlm { request: {"kind": "deliberate", ...} }` emitted by
//! the SDLC machine.  Each deliberation runs a state-scoped model↔tool agentic
//! loop (via [`sven_core::Deliberator`]) against that state's **append-only**
//! conversation thread (stored in the long-lived [`ConversationStore`]), with a
//! state-scoped tool subset, an optional per-state model, and a structured
//! decision schema.
//!
//! When the loop concludes, the final tool-free text is parsed (with code-fence
//! stripping) into a JSON decision and posted inward as
//! [`Event::DeliberationComplete`]; the raw JSON never reaches the observation
//! plane.  Streaming text/thinking/tool observations are bridged to [`UiEvent`]s
//! while the loop runs, reusing the converse executor's mapping.
//!
//! # Cancellation
//!
//! Like [`crate::ConverseExecutor`], a shared cancel slot lets the TUI abort the
//! in-flight deliberation.

use std::sync::Arc;

use async_trait::async_trait;
use sven_core::{to_model_schemas, DeliberationParams, Deliberator, ModelResolver};
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink, UiEvent};
use sven_llm::{strip_code_fences, ConversationStore, DeliberationRequest};
use sven_tools::ToolRegistry;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::converse::agent_event_to_ui;

/// The JSON `kind` tag that selects the deliberation engine.
pub use sven_llm::DELIBERATE_KIND;

/// Default per-tool-result token cap applied when truncating tool outputs.
const DEFAULT_TOOL_RESULT_CAP: usize = 8_000;

/// Drives SDLC deliberations against a long-lived conversation store.
pub struct DeliberationExecutor {
    /// Default model used when a request does not name a per-state override.
    default_model: Arc<dyn sven_model::ModelProvider>,
    /// Resolver for per-state model overrides (falls back to default on error).
    model_resolver: Option<ModelResolver>,
    /// Shared tool registry; state-scoped subsets are selected per deliberation.
    tools: Arc<ToolRegistry>,
    /// Append-only per-thread conversation history, owned for the runtime.
    store: ConversationStore,
    /// Shared cancel slot the TUI uses to abort the in-flight deliberation.
    cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl DeliberationExecutor {
    /// Create a deliberation executor.
    pub fn new(
        default_model: Arc<dyn sven_model::ModelProvider>,
        model_resolver: Option<ModelResolver>,
        tools: Arc<ToolRegistry>,
        cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    ) -> Self {
        Self {
            default_model,
            model_resolver,
            tools,
            store: ConversationStore::new(),
            cancel_handle,
        }
    }

    /// Resolve the model provider for this deliberation, honouring a per-state
    /// override when both an override and a resolver are present.
    fn resolve_model(&self, model: &Option<String>) -> Arc<dyn sven_model::ModelProvider> {
        if let (Some(name), Some(resolver)) = (model.as_ref(), self.model_resolver.as_ref()) {
            match resolver(name) {
                Ok(m) => return m,
                Err(e) => {
                    tracing::warn!(model = %name, error = %e, "deliberation: model override failed; using default");
                }
            }
        }
        Arc::clone(&self.default_model)
    }
}

#[async_trait]
impl EffectExecutor for DeliberationExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallLlm { request } = effect else {
            return;
        };

        if !DeliberationRequest::is_deliberation(&request) {
            tracing::warn!("DeliberationExecutor received a non-deliberation CallLlm; ignoring");
            let _ = sink
                .emit(Event::LlmFailed {
                    error: "deliberation executor received a non-deliberation request".into(),
                })
                .await;
            obs.emit(UiEvent::TurnComplete);
            return;
        }

        let req = match DeliberationRequest::from_value(request) {
            Ok(r) => r,
            Err(e) => {
                let _ = sink
                    .emit(Event::LlmFailed {
                        error: format!("failed to deserialise DeliberationRequest: {e}"),
                    })
                    .await;
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        let thread_id = req.thread.clone();
        let model = self.resolve_model(&req.model);

        // Resolve the state-scoped tool subset against the live registry.
        let tool_schemas = to_model_schemas(self.tools.schemas_for_names(&req.tools));

        let response_format = if req.schema.is_null() {
            None
        } else {
            Some(sven_model::ResponseFormat::JsonSchema {
                name: if req.schema_name.is_empty() {
                    "decision".to_string()
                } else {
                    req.schema_name.clone()
                },
                schema: req.schema.clone(),
            })
        };

        let params = DeliberationParams {
            system_role: req.system_role.clone(),
            instruction: req.instruction.clone(),
            tools: tool_schemas,
            response_format,
            max_tool_rounds: req.max_tool_rounds.unwrap_or(16),
            tool_result_token_cap: DEFAULT_TOOL_RESULT_CAP,
        };

        // Bridge AgentEvents → UiEvents while the loop runs.  TextComplete is
        // intentionally NOT forwarded to the UI: the final turn is the raw
        // structured decision and must not leak to the observation plane.
        let (tx, mut rx) = mpsc::channel::<sven_core::AgentEvent>(256);
        let obs_fwd = obs.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, sven_core::AgentEvent::TextComplete(_)) {
                    continue;
                }
                if let Some(ui) = agent_event_to_ui(ev) {
                    obs_fwd.emit(ui);
                }
            }
        });

        // Install a fresh cancel channel.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *self.cancel_handle.lock().await = Some(cancel_tx);

        let deliberator = Deliberator::new(model, Arc::clone(&self.tools));
        let thread = self.store.thread(&thread_id);
        let result = deliberator.run(thread, params, tx, cancel_rx).await;

        self.cancel_handle.lock().await.take();
        let _ = forwarder.await;

        match result {
            Ok(final_text) => match parse_decision(&final_text) {
                Ok(decision) => {
                    let _ = sink
                        .emit(Event::DeliberationComplete {
                            thread: thread_id,
                            decision,
                        })
                        .await;
                }
                Err(e) => {
                    let msg = format!("deliberation produced unparsable decision: {e}");
                    tracing::warn!(error = %msg, raw = %final_text);
                    obs.emit(UiEvent::Error(msg.clone()));
                    let _ = sink.emit(Event::LlmFailed { error: msg }).await;
                }
            },
            Err(e) => {
                let msg = format!("{e:#}");
                obs.emit(UiEvent::Error(msg.clone()));
                let _ = sink.emit(Event::LlmFailed { error: msg }).await;
            }
        }

        obs.emit(UiEvent::TurnComplete);
    }
}

/// Parse the model's final text into a JSON decision, tolerating code fences
/// and trailing prose around the object.
fn parse_decision(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    let stripped = strip_code_fences(raw);
    match serde_json::from_str::<serde_json::Value>(stripped) {
        Ok(v) => Ok(v),
        Err(first_err) => {
            // Fall back to the first balanced { .. } object in the text.
            if let Some(obj) = extract_first_json_object(stripped) {
                serde_json::from_str::<serde_json::Value>(&obj)
            } else {
                Err(first_err)
            }
        }
    }
}

/// Extract the first balanced top-level `{ .. }` substring, ignoring braces
/// inside JSON strings.  Returns `None` when no balanced object is present.
fn extract_first_json_object(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sven_hsm::{
        Context, Hsm, Machine, MachineId, PermissionPolicy, Reaction, Runtime,
    };
    use sven_model::{CompletionRequest, ModelProvider, ResponseEvent};

    #[test]
    fn parse_decision_plain_json() {
        let v = parse_decision(r#"{"status":"proceed"}"#).unwrap();
        assert_eq!(v["status"], "proceed");
    }

    #[test]
    fn parse_decision_strips_fences() {
        let v = parse_decision("```json\n{\"status\":\"failed\"}\n```").unwrap();
        assert_eq!(v["status"], "failed");
    }

    #[test]
    fn parse_decision_extracts_object_amid_prose() {
        let v =
            parse_decision("Here is my decision:\n{\"status\":\"need_approval\"}\nThanks!").unwrap();
        assert_eq!(v["status"], "need_approval");
    }

    #[test]
    fn parse_decision_handles_braces_in_strings() {
        let v = parse_decision(r#"prefix {"note":"a } b","status":"proceed"} suffix"#).unwrap();
        assert_eq!(v["status"], "proceed");
        assert_eq!(v["note"], "a } b");
    }

    /// Streams a fixed decision then Done.
    struct DecideProvider(&'static str);
    #[async_trait]
    impl ModelProvider for DecideProvider {
        fn name(&self) -> &str {
            "decide"
        }
        fn model_name(&self) -> &str {
            "decide"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let events: Vec<anyhow::Result<ResponseEvent>> = vec![
                Ok(ResponseEvent::TextDelta(self.0.into())),
                Ok(ResponseEvent::Done),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

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
                    if let Event::DeliberationComplete { thread, decision } = e {
                        ctx.set_fact("thread", json!(thread));
                        ctx.set_fact("decision", decision.clone());
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

    #[tokio::test]
    async fn deliberation_emits_decision_complete() {
        let mut exec = DeliberationExecutor::new(
            Arc::new(DecideProvider("{\"status\":\"proceed\"}")),
            None,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mutex::new(None)),
        );

        let rt = Runtime::spawn(
            Hsm::new(OneShot(MachineId::new())),
            Context::new(),
            PermissionPolicy::builder().build(),
            crate::CompositeExecutor::builder().build(),
            16,
        );
        let sink = rt.sink();
        let obs = ObservationSink::new(64);

        let req = DeliberationRequest {
            thread: "intake".into(),
            system_role: "role".into(),
            instruction: "decide".into(),
            ..Default::default()
        };
        exec.execute(
            Effect::CallLlm {
                request: req.to_value(),
            },
            &sink,
            &obs,
        )
        .await;

        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("DeliberationComplete")
        );
        assert_eq!(
            report.ctx.fact("thread").and_then(|v| v.as_str()),
            Some("intake")
        );
        assert_eq!(
            report
                .ctx
                .fact("decision")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("proceed")
        );
    }

    #[tokio::test]
    async fn non_deliberation_request_fails_gracefully() {
        let mut exec = DeliberationExecutor::new(
            Arc::new(DecideProvider("{}")),
            None,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mutex::new(None)),
        );
        let rt = Runtime::spawn(
            Hsm::new(OneShot(MachineId::new())),
            Context::new(),
            PermissionPolicy::builder().build(),
            crate::CompositeExecutor::builder().build(),
            16,
        );
        let sink = rt.sink();
        let obs = ObservationSink::new(8);
        exec.execute(
            Effect::CallLlm {
                request: json!({"kind": "converse", "text": "x"}),
            },
            &sink,
            &obs,
        )
        .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("LlmFailed")
        );
    }

    #[tokio::test]
    async fn thread_persists_across_deliberations_append_only() {
        // Two deliberations on the same thread should accumulate turns.
        let mut exec = DeliberationExecutor::new(
            Arc::new(DecideProvider("{\"status\":\"proceed\"}")),
            None,
            Arc::new(ToolRegistry::new()),
            Arc::new(Mutex::new(None)),
        );
        let obs = ObservationSink::new(64);

        for _ in 0..2 {
            let rt = Runtime::spawn(
                Hsm::new(OneShot(MachineId::new())),
                Context::new(),
                PermissionPolicy::builder().build(),
                crate::CompositeExecutor::builder().build(),
                16,
            );
            let sink = rt.sink();
            let req = DeliberationRequest {
                thread: "discovery".into(),
                system_role: "role".into(),
                instruction: "explore".into(),
                ..Default::default()
            };
            exec.execute(
                Effect::CallLlm {
                    request: req.to_value(),
                },
                &sink,
                &obs,
            )
            .await;
            rt.wait_done().await;
            rt.join().await.unwrap();
        }

        // system + (user + assistant) * 2 = 5 turns, append-only.
        assert_eq!(exec.store.len("discovery"), 5);
    }
}
