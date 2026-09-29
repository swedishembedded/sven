//! [`CompositeExecutor`] — the single `EffectExecutor` wired to the runtime.
//!
//! Dispatches each [`Effect`] to the appropriate sub-executor based on its
//! variant.  All `CallLlm` effects must carry `kind="turn"` (handled by
//! [`TurnExecutor`]).  Other variants log a warning and no-op.
//!
//! # Wiring
//!
//! ```rust,ignore
//! let executor = CompositeExecutor::builder()
//!     .with_turn(TurnExecutor::new(model, registry, conv_store, call_registry, cancel_handle))
//!     .with_tools(registry, HashSet::new())
//!     .with_user(q_tx, a_tx)
//!     .with_timers(Arc::new(SystemClock::new()))
//!     .with_audit("/var/log/sven/audit.jsonl")
//!     .build();
//!
//! let runtime = Runtime::spawn(Hsm::new(machine), ctx, policy, executor, 64);
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{AuditTrailHandle, Effect, EffectKind, ObservationSink};
use sven_kernel::{Clock, EffectExecutor, EventSink};
use sven_tool_registry::ToolRegistry;
use tokio::sync::mpsc;

use crate::audit::AuditExecutor;
use crate::internal::InternalExecutor;
use crate::timer::TimerExecutor;
use crate::tool::ToolExecutor;
use crate::turn::TurnExecutor;
use crate::user::{ApprovalRequest, UserExecutor, UserQuestion};
use crate::verify::VerifyExecutor;

/// All sub-executors collected into one structure.
///
/// Each slot holds a `Box<dyn EffectExecutor>` so any slot can be substituted
/// with a custom executor via the `with_*_slot` builder methods; the concrete
/// `with_*` methods wire the default executors.
pub struct CompositeExecutor {
    /// Single-turn engine (kernel-native loop). Handles `CallLlm` effects
    /// whose request `kind` is `"turn"`.
    turn: Option<Box<dyn EffectExecutor>>,
    tool: Option<Box<dyn EffectExecutor>>,
    user: Option<Box<dyn EffectExecutor>>,
    timer: Option<Box<dyn EffectExecutor>>,
    audit: Option<Box<dyn EffectExecutor>>,
    internal: Box<dyn EffectExecutor>,
    verify: Option<Box<dyn EffectExecutor>>,
}

impl CompositeExecutor {
    /// Starts building a [`CompositeExecutor`].
    pub fn builder() -> CompositeExecutorBuilder {
        CompositeExecutorBuilder::default()
    }
}

#[async_trait]
impl EffectExecutor for CompositeExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let kind = effect.kind();
        match kind {
            EffectKind::CallLlm => {
                let request_kind = match &effect {
                    Effect::CallLlm { request } => request
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    _ => None,
                };
                let is_turn = request_kind.as_deref() == Some(sven_vocab::TURN_KIND);
                if is_turn {
                    if let Some(exec) = &mut self.turn {
                        exec.execute(effect, sink, obs).await;
                    } else {
                        tracing::warn!(
                            "CompositeExecutor: no Turn executor configured; dropping turn CallLlm"
                        );
                    }
                } else {
                    tracing::warn!(
                        kind = ?request_kind,
                        "CompositeExecutor: unrecognised CallLlm kind; dropping (use kind=turn)"
                    );
                }
            }

            EffectKind::CallTool => {
                if let Some(exec) = &mut self.tool {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(
                        "CompositeExecutor: no Tool executor configured; dropping CallTool"
                    );
                }
            }

            EffectKind::AskUser
            | EffectKind::RequestHumanApproval
            | EffectKind::RequestHumanAnswer => {
                if let Some(exec) = &mut self.user {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(
                        ?kind,
                        "CompositeExecutor: no User executor configured; dropping user effect"
                    );
                }
            }

            EffectKind::ScheduleTimeout | EffectKind::CancelTimeout => {
                if let Some(exec) = &mut self.timer {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(
                        ?kind,
                        "CompositeExecutor: no Timer executor configured; dropping timer effect"
                    );
                }
            }

            EffectKind::PersistAudit => {
                if let Some(exec) = &mut self.audit {
                    exec.execute(effect, sink, obs).await;
                }
                // Silently ignore when not configured.
            }

            EffectKind::EmitInternal => {
                self.internal.execute(effect, sink, obs).await;
            }

            EffectKind::InstantiateSubmachine => {
                tracing::warn!("CompositeExecutor: InstantiateSubmachine not yet wired; ignored");
            }

            EffectKind::Verify => {
                if let Some(exec) = &mut self.verify {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(
                        "CompositeExecutor: no Verify executor configured; dropping Verify effect"
                    );
                }
            }
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builds a [`CompositeExecutor`] by composing sub-executors incrementally.
///
/// The `with_*` methods attach the default concrete executors; the
/// `with_*_slot` methods substitute any [`EffectExecutor`] into a slot
/// (used to inject custom executors, e.g. in tests or embeddings).
#[derive(Default)]
pub struct CompositeExecutorBuilder {
    turn: Option<Box<dyn EffectExecutor>>,
    tool: Option<Box<dyn EffectExecutor>>,
    user: Option<Box<dyn EffectExecutor>>,
    timer: Option<Box<dyn EffectExecutor>>,
    audit: Option<Box<dyn EffectExecutor>>,
    internal: Option<Box<dyn EffectExecutor>>,
    verify: Option<Box<dyn EffectExecutor>>,
}

impl CompositeExecutorBuilder {
    /// Attach the single-turn engine (kernel-native loop).
    ///
    /// `CallLlm` effects with `kind="turn"` are handled by `TurnExecutor`,
    /// which streams a single model response, appends to the
    /// `ThreadStore`, and posts `LlmTurnComplete`.
    pub fn with_turn(mut self, exec: TurnExecutor) -> Self {
        self.turn = Some(Box::new(exec));
        self
    }

    /// Attach the tool executor with the given registry and capability allow-list.
    ///
    /// This creates a **fresh** `ToolExecutor` with its own empty `call_id_to_thread`
    /// registry.  If you need tool results to be appended to the same conversation
    /// thread that `TurnExecutor` is writing to, use [`Self::with_tool_executor`]
    /// and pass a pre-wired [`ToolExecutor::with_shared_store`] instance instead.
    pub fn with_tools(
        mut self,
        registry: Arc<ToolRegistry>,
        allowed_capabilities: HashSet<sven_hsm::ToolCapability>,
    ) -> Self {
        self.tool = Some(Box::new(ToolExecutor::new(registry, allowed_capabilities)));
        self
    }

    /// Attach a pre-built [`ToolExecutor`].
    ///
    /// Use this when the `ToolExecutor` must share its `call_id_to_thread`
    /// registry and `ThreadStore` with a `TurnExecutor` so tool results
    /// are appended under the correct thread before the continuation LLM call.
    pub fn with_tool_executor(mut self, exec: ToolExecutor) -> Self {
        self.tool = Some(Box::new(exec));
        self
    }

    /// Attach the user/approval executor with pre-built channels.
    pub fn with_user(
        mut self,
        question_tx: mpsc::Sender<UserQuestion>,
        approval_tx: mpsc::Sender<ApprovalRequest>,
    ) -> Self {
        self.user = Some(Box::new(UserExecutor::new(question_tx, approval_tx)));
        self
    }

    /// Attach the timer executor backed by `clock`.
    pub fn with_timers(mut self, clock: Arc<dyn Clock>) -> Self {
        self.timer = Some(Box::new(TimerExecutor::new(clock)));
        self
    }

    /// Attach the verifier executor, jailing every `Effect::Verify` to `root`.
    pub fn with_verify(mut self, root: impl Into<PathBuf>) -> Self {
        self.verify = Some(Box::new(VerifyExecutor::new(root)));
        self
    }

    /// Attach the audit executor writing to `log_path`.
    ///
    /// Without a trail each `PersistAudit` appends a hash-chained checkpoint
    /// marker only; use [`Self::with_audit_trail`] to persist the full audit
    /// records.
    pub fn with_audit(mut self, log_path: impl Into<PathBuf>) -> Self {
        self.audit = Some(Box::new(AuditExecutor::new(log_path)));
        self
    }

    /// Attach the audit executor writing the full hash-chained audit records
    /// mirrored in `trail` to `log_path`.
    ///
    /// Share the same [`AuditTrailHandle`] with the runtime (see
    /// `sven_kernel::ErasedRuntime::spawn_with_audit_trail`) so `PersistAudit`
    /// flushes every dispatch and tool audit record accumulated since the
    /// previous flush.
    pub fn with_audit_trail(
        mut self,
        log_path: impl Into<PathBuf>,
        trail: AuditTrailHandle,
    ) -> Self {
        self.audit = Some(Box::new(AuditExecutor::with_trail(log_path, trail)));
        self
    }

    /// Substitute a custom executor into the turn slot (`CallLlm` with
    /// `kind="turn"`).
    pub fn with_turn_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.turn = Some(exec);
        self
    }

    /// Substitute a custom executor into the tool slot (`CallTool`).
    pub fn with_tool_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.tool = Some(exec);
        self
    }

    /// Substitute a custom executor into the user slot (`AskUser`,
    /// `RequestHumanApproval`).
    pub fn with_user_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.user = Some(exec);
        self
    }

    /// Substitute a custom executor into the timer slot (`ScheduleTimeout`,
    /// `CancelTimeout`).
    pub fn with_timer_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.timer = Some(exec);
        self
    }

    /// Substitute a custom executor into the audit slot (`PersistAudit`).
    pub fn with_audit_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.audit = Some(exec);
        self
    }

    /// Substitute a custom executor into the verify slot (`Verify`).
    pub fn with_verify_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.verify = Some(exec);
        self
    }

    /// Substitute a custom executor into the internal slot (`EmitInternal`).
    /// Defaults to [`InternalExecutor`] when not set.
    pub fn with_internal_slot(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.internal = Some(exec);
        self
    }

    /// Finalise and return the [`CompositeExecutor`].
    pub fn build(self) -> CompositeExecutor {
        CompositeExecutor {
            turn: self.turn,
            tool: self.tool,
            user: self.user,
            timer: self.timer,
            audit: self.audit,
            internal: self
                .internal
                .unwrap_or_else(|| Box::new(InternalExecutor::new())),
            verify: self.verify,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use sven_hsm::{Context, Effect, Event, Hsm, MachineId, PermissionPolicy, Reaction};
    use sven_kernel::{EffectExecutor, Runtime};
    use sven_tool_registry::ToolRegistry;

    use super::{CompositeExecutor, CompositeExecutorBuilder};
    use crate::user::{ApprovalRequest, UserQuestion};

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct OneShotMachine(MachineId);
    impl OneShotMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }
    impl sven_hsm::Machine for OneShotMachine {
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
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        return Reaction::Handled(vec![]);
                    }
                    ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    async fn run_composite_effect(exec: &mut CompositeExecutor, effect: Effect) -> String {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            super::CompositeExecutor::builder().build(),
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report
            .ctx
            .fact("received_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "no event received".into())
    }

    #[tokio::test]
    async fn composite_routes_emit_internal_to_internal_executor() {
        let mut exec = CompositeExecutorBuilder::default().build();
        let effect = Effect::EmitInternal {
            name: "test_signal".into(),
            payload: json!({}),
        };
        let kind = run_composite_effect(&mut exec, effect).await;
        assert_eq!(kind, "Custom");
    }

    #[tokio::test]
    async fn composite_no_llm_executor_logs_and_noops() {
        let mut exec = CompositeExecutorBuilder::default().build();
        let effect = Effect::CallLlm {
            request: json!({"kind": "unknown_kind_xyz"}),
        };
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            CompositeExecutorBuilder::default().build(),
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !rt.status().done,
            "machine should remain in Idle with no executor"
        );
        rt.abort();
    }

    /// Records every effect it receives; used to prove custom executors can
    /// be substituted into a composite slot.
    struct RecordingExecutor {
        effects: Arc<std::sync::Mutex<Vec<Effect>>>,
    }

    #[async_trait::async_trait]
    impl EffectExecutor for RecordingExecutor {
        async fn execute(
            &mut self,
            effect: Effect,
            _sink: &sven_kernel::EventSink,
            _obs: &sven_hsm::ObservationSink,
        ) {
            self.effects.lock().unwrap().push(effect);
        }
    }

    #[tokio::test]
    async fn custom_executor_in_slot_receives_effects() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut exec = CompositeExecutorBuilder::default()
            .with_tool_slot(Box::new(RecordingExecutor {
                effects: Arc::clone(&recorded),
            }))
            .build();

        let effect = Effect::CallTool {
            call_id: sven_hsm::ToolCallId::new(),
            name: "some_tool".into(),
            args: json!({"x": 1}),
            capability: sven_hsm::ToolCapability::ReadFile,
        };

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            CompositeExecutorBuilder::default().build(),
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        rt.abort();

        let effects = recorded.lock().unwrap();
        assert_eq!(
            effects.len(),
            1,
            "custom slot executor should receive the effect"
        );
        assert!(
            matches!(&effects[0], Effect::CallTool { name, .. } if name == "some_tool"),
            "expected the CallTool effect, got {:?}",
            effects[0].kind()
        );
    }

    #[tokio::test]
    async fn composite_routes_call_tool_to_tool_executor() {
        let (q_tx, _q_rx) = tokio::sync::mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = tokio::sync::mpsc::channel::<ApprovalRequest>(4);

        let effect = Effect::CallTool {
            call_id: sven_hsm::ToolCallId::new(),
            name: "nonexistent_tool".into(),
            args: json!({}),
            capability: sven_hsm::ToolCapability::ReadFile,
        };

        let mut exec = CompositeExecutorBuilder::default()
            .with_user(q_tx, a_tx)
            .with_tools(Arc::new(ToolRegistry::new()), Default::default())
            .build();

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            super::CompositeExecutorBuilder::default().build(),
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rt.abort();
    }
}
