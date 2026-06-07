//! [`CompositeExecutor`] — the single `EffectExecutor` wired to the runtime.
//!
//! Dispatches each [`Effect`] to the appropriate sub-executor based on its
//! variant. Unhandled variants log a warning and no-op. All sub-executors are
//! owned by the `CompositeExecutor` so there is exactly one instance running
//! per kernel (respecting run-to-completion).
//!
//! # Wiring
//!
//! ```rust,ignore
//! let executor = CompositeExecutor::builder()
//!     .with_turn(TurnExecutor::new(model, registry, conv_store, call_registry, cancel_handle))
//!     .with_tools(registry, HashSet::new())
//!     .with_user(q_tx, a_tx)
//!     .with_timers(Arc::new(SystemClock::new()))
//!     .with_checkpoints("/path/to/repo")
//!     .with_audit("/var/log/sven/audit.jsonl")
//!     .build();
//!
//! let runtime = Runtime::spawn(Hsm::new(machine), ctx, policy, executor, 64);
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Clock, Effect, EffectExecutor, EffectKind, EventSink, ObservationSink};
use sven_tools::ToolRegistry;
use tokio::sync::mpsc;

use crate::audit::AuditExecutor;
use crate::checkpoint::CheckpointExecutor;
use crate::converse::ConverseExecutor;
use crate::deliberation::DeliberationExecutor;
use crate::internal::InternalExecutor;
use crate::timer::TimerExecutor;
use crate::tool::ToolExecutor;
use crate::turn::TurnExecutor;
use crate::user::{ApprovalRequest, UserExecutor, UserQuestion};

/// All sub-executors collected into one structure.
pub struct CompositeExecutor {
    /// Converse turn engine (reactive agent). When present it handles
    /// `CallLlm` effects whose request `kind` is `"converse"`.
    converse: Option<ConverseExecutor>,
    /// Deliberation engine (SDLC mode). When present it handles `CallLlm`
    /// effects whose request `kind` is `"deliberate"`.
    deliberation: Option<DeliberationExecutor>,
    /// Single-turn engine (kernel-native loop). When present it handles
    /// `CallLlm` effects whose request `kind` is `"turn"`.
    turn: Option<TurnExecutor>,
    tool: Option<ToolExecutor>,
    user: Option<UserExecutor>,
    timer: Option<TimerExecutor>,
    checkpoint: Option<CheckpointExecutor>,
    audit: Option<AuditExecutor>,
    internal: InternalExecutor,
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
                // Route converse-kind requests to the converse engine when one
                // is configured; everything else goes to the typed LLM adapter.
                let request_kind = match &effect {
                    Effect::CallLlm { request } => {
                        request.get("kind").and_then(|v| v.as_str()).map(str::to_string)
                    }
                    _ => None,
                };
                let is_converse = request_kind.as_deref() == Some(crate::converse::CONVERSE_KIND);
                let is_deliberate =
                    request_kind.as_deref() == Some(crate::deliberation::DELIBERATE_KIND);
                let is_turn = request_kind.as_deref() == Some(crate::turn::TURN_KIND);
                if is_turn {
                    if let Some(exec) = &mut self.turn {
                        exec.execute(effect, sink, obs).await;
                    } else {
                        tracing::warn!(
                            "CompositeExecutor: no Turn executor configured; dropping turn CallLlm"
                        );
                    }
                } else if is_converse {
                    if let Some(exec) = &mut self.converse {
                        exec.execute(effect, sink, obs).await;
                    } else {
                        tracing::warn!(
                            "CompositeExecutor: no Converse executor configured; dropping converse CallLlm"
                        );
                    }
                } else if is_deliberate {
                    if let Some(exec) = &mut self.deliberation {
                        exec.execute(effect, sink, obs).await;
                    } else {
                        tracing::warn!(
                            "CompositeExecutor: no Deliberation executor configured; dropping deliberate CallLlm"
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

            EffectKind::AskUser | EffectKind::RequestHumanApproval => {
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

            EffectKind::CreateCheckpoint | EffectKind::RollbackToCheckpoint => {
                if let Some(exec) = &mut self.checkpoint {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(?kind, "CompositeExecutor: no Checkpoint executor configured; dropping checkpoint effect");
                }
            }

            EffectKind::PersistAudit => {
                if let Some(exec) = &mut self.audit {
                    exec.execute(effect, sink, obs).await;
                } else {
                    // Silently ignore; not all deployments need file audit logs.
                }
            }

            EffectKind::EmitInternal => {
                self.internal.execute(effect, sink, obs).await;
            }

            EffectKind::InstantiateSubmachine => {
                // Submachine instantiation is handled by the runtime layer.
                // The executor just warns; a real implementation would look up
                // a machine factory and wire it into the runtime's submachine slot.
                tracing::warn!("CompositeExecutor: InstantiateSubmachine not yet wired; ignored");
            }
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builds a [`CompositeExecutor`] by composing sub-executors incrementally.
#[derive(Default)]
pub struct CompositeExecutorBuilder {
    converse: Option<ConverseExecutor>,
    deliberation: Option<DeliberationExecutor>,
    turn: Option<TurnExecutor>,
    tool: Option<ToolExecutor>,
    user: Option<UserExecutor>,
    timer: Option<TimerExecutor>,
    checkpoint: Option<CheckpointExecutor>,
    audit: Option<AuditExecutor>,
}

impl CompositeExecutorBuilder {
    /// Attach the converse turn engine backed by a shared [`sven_core::Agent`].
    ///
    /// When present, `CallLlm` effects whose request `kind` is `"converse"`
    /// (emitted by `ReactiveAgentMachine`) are driven through the full legacy
    /// agentic loop with streaming observations.
    ///
    /// `cancel_handle` is the shared abort slot the TUI uses to cancel an
    /// in-flight turn. Pass the same `Arc` that `App::agent.cancel` points to
    /// so the TUI's `/abort` command reaches the executor directly.
    pub fn with_converse(
        mut self,
        agent: Arc<tokio::sync::Mutex<sven_core::Agent>>,
        cancel_handle: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    ) -> Self {
        self.converse = Some(ConverseExecutor::new(agent, cancel_handle));
        self
    }

    /// Attach the deliberation engine (SDLC mode).
    ///
    /// When present, `CallLlm` effects whose request `kind` is `"deliberate"`
    /// run a state-scoped model↔tool agentic loop against an append-only
    /// conversation thread and post `DeliberationComplete` back to the machine.
    pub fn with_deliberation(mut self, exec: DeliberationExecutor) -> Self {
        self.deliberation = Some(exec);
        self
    }

    /// Attach the single-turn engine (kernel-native loop).
    ///
    /// When present, `CallLlm` effects whose request `kind` is `"turn"` are
    /// handled by `TurnExecutor`, which streams a single model response,
    /// appends to the ConversationStore, and posts `LlmTurnComplete`.
    pub fn with_turn(mut self, exec: TurnExecutor) -> Self {
        self.turn = Some(exec);
        self
    }

    /// Attach the tool executor with the given registry and capability allow-list.
    pub fn with_tools(
        mut self,
        registry: Arc<ToolRegistry>,
        allowed_capabilities: HashSet<sven_hsm::ToolCapability>,
    ) -> Self {
        self.tool = Some(ToolExecutor::new(registry, allowed_capabilities));
        self
    }

    /// Attach the user/approval executor with pre-built channels.
    pub fn with_user(
        mut self,
        question_tx: mpsc::Sender<UserQuestion>,
        approval_tx: mpsc::Sender<ApprovalRequest>,
    ) -> Self {
        self.user = Some(UserExecutor::new(question_tx, approval_tx));
        self
    }

    /// Attach the timer executor backed by `clock`.
    pub fn with_timers(mut self, clock: Arc<dyn Clock>) -> Self {
        self.timer = Some(TimerExecutor::new(clock));
        self
    }

    /// Attach the checkpoint executor operating in `repo_dir`.
    pub fn with_checkpoints(mut self, repo_dir: impl Into<PathBuf>) -> Self {
        self.checkpoint = Some(CheckpointExecutor::new(repo_dir));
        self
    }

    /// Attach the audit executor writing to `log_path`.
    pub fn with_audit(mut self, log_path: impl Into<PathBuf>) -> Self {
        self.audit = Some(AuditExecutor::new(log_path));
        self
    }

    /// Finalise and return the [`CompositeExecutor`].
    pub fn build(self) -> CompositeExecutor {
        CompositeExecutor {
            converse: self.converse,
            deliberation: self.deliberation,
            turn: self.turn,
            tool: self.tool,
            user: self.user,
            timer: self.timer,
            checkpoint: self.checkpoint,
            audit: self.audit,
            internal: InternalExecutor::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, Hsm, MachineId, PermissionPolicy, Reaction, Runtime,
    };
    use sven_tools::ToolRegistry;

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
        // Without a turn executor, CallLlm with unrecognised kind is dropped
        // and the machine stays in Idle.
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

        // Give it a moment to propagate (it should not).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !rt.status().done,
            "machine should remain in Idle with no executor"
        );
        rt.abort();
    }

    /// Tools must still be callable even when no LLM executor is wired.
    #[tokio::test]
    async fn composite_routes_call_tool_to_tool_executor() {
        let (q_tx, _q_rx) = tokio::sync::mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = tokio::sync::mpsc::channel::<ApprovalRequest>(4);

        // Tool calls route through the tool executor even if no LLM is wired.
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

        // Just ensure no panic; the registry has no tools so ToolFailed is emitted.
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
