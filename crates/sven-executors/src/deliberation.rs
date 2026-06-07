// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Deliberation effect executor — legacy stub.
//!
//! This module previously ran an internal Deliberator loop.  It has been
//! replaced by the kernel-native [`crate::TurnExecutor`] path: the SDLC
//! machine now emits `Effect::CallLlm { kind: "turn" }` instead of
//! `kind: "deliberate"`, and `TurnExecutor` streams the turn while the
//! kernel dispatches tool calls through `Effect::CallTool`.
//!
//! This stub is retained solely for backward-compat wiring in
//! `sven-bootstrap` until that crate is updated in Phase D2.  Any
//! `kind="deliberate"` effect that still arrives here is dropped with a
//! warning so existing builds do not hard-fail.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink, UiEvent};
use sven_tools::ToolRegistry;
use tokio::sync::{oneshot, Mutex};

/// The JSON `kind` tag that previously selected the deliberation engine.
pub use sven_llm::DELIBERATE_KIND;

/// Legacy stub for the deliberation engine.
///
/// Wired by `sven-bootstrap` until it is updated to use [`crate::TurnExecutor`].
/// All `kind="deliberate"` effects are rejected with `Event::LlmFailed`.
pub struct DeliberationExecutor {
    _tools: Arc<ToolRegistry>,
    _cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl DeliberationExecutor {
    /// Create a stub deliberation executor (all fields ignored).
    pub fn new(
        _default_model: Arc<dyn sven_model::ModelProvider>,
        _model_resolver: Option<sven_core::ModelResolver>,
        tools: Arc<ToolRegistry>,
        cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    ) -> Self {
        Self {
            _tools: tools,
            _cancel_handle: cancel_handle,
        }
    }
}

#[async_trait]
impl EffectExecutor for DeliberationExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallLlm { .. } = effect else {
            return;
        };
        tracing::warn!(
            "DeliberationExecutor: deliberation loop removed in Phase D; \
             SDLC machine should emit kind=\"turn\" instead of kind=\"deliberate\". \
             Emitting LlmFailed so the machine can route to recovery."
        );
        let _ = sink
            .emit(Event::LlmFailed {
                error: "deliberation loop removed; use TurnExecutor".into(),
            })
            .await;
        obs.emit(UiEvent::TurnComplete);
    }
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
        let raw = r#"{"status":"proceed"}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(v["status"], "proceed");
    }

    #[test]
    fn parse_decision_strips_fences() {
        let stripped = sven_llm::strip_code_fences("```json\n{\"status\":\"failed\"}\n```");
        let v: serde_json::Value = serde_json::from_str(stripped).unwrap();
        assert_eq!(v["status"], "failed");
    }

    struct DecideProvider(&'static str);
    #[async_trait]
    impl ModelProvider for DecideProvider {
        fn name(&self) -> &str { "decide" }
        fn model_name(&self) -> &str { "decide" }
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
    enum TS { Top, Idle, Done }
    struct OneShot(MachineId);
    impl Machine for OneShot {
        type State = TS;
        fn id(&self) -> MachineId { self.0 }
        fn top(&self) -> TS { TS::Top }
        fn initial(&self) -> TS { TS::Idle }
        fn superstate(&self, _s: TS) -> TS { TS::Top }
        fn is_terminal(&self, s: TS) -> bool { s == TS::Done }
        fn dispatch_state(&mut self, s: TS, e: &sven_hsm::Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Idle if !e.is_lifecycle() => {
                    ctx.set_fact("kind", format!("{:?}", e.kind()));
                    Reaction::Transition { target: TS::Done, effects: vec![], rationale: "got event".into() }
                }
                _ => Reaction::Handled(vec![]),
            }
        }
    }

    #[tokio::test]
    async fn deliberation_emits_decision_complete() {
        // Stub now emits LlmFailed instead of DeliberationComplete.
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

        let req = sven_llm::DeliberationRequest {
            thread: "intake".into(),
            system_role: "role".into(),
            instruction: "decide".into(),
            ..Default::default()
        };
        exec.execute(Effect::CallLlm { request: req.to_value() }, &sink, &obs).await;

        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        // Stub emits LlmFailed (not DeliberationComplete).
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("LlmFailed")
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
            Effect::CallLlm { request: json!({"kind": "deliberate", "text": "x"}) },
            &sink,
            &obs,
        ).await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report.ctx.fact("kind").and_then(|v| v.as_str()),
            Some("LlmFailed")
        );
    }

    #[tokio::test]
    async fn thread_persists_across_deliberations_append_only() {
        // Stub doesn't maintain a thread; this test just verifies it doesn't panic.
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
            let req = sven_llm::DeliberationRequest {
                thread: "discovery".into(),
                system_role: "role".into(),
                instruction: "explore".into(),
                ..Default::default()
            };
            exec.execute(Effect::CallLlm { request: req.to_value() }, &sink, &obs).await;
            rt.wait_done().await;
            rt.join().await.unwrap();
        }
    }
}
