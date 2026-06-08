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
use crate::internal::InternalExecutor;
use crate::timer::TimerExecutor;
use crate::tool::ToolExecutor;
use crate::turn::TurnExecutor;
use crate::user::{ApprovalRequest, UserExecutor, UserQuestion};

/// All sub-executors collected into one structure.
pub struct CompositeExecutor {
    /// Single-turn engine (kernel-native loop). Handles `CallLlm` effects
    /// whose request `kind` is `"turn"`.
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
                let request_kind = match &effect {
                    Effect::CallLlm { request } => {
                        request.get("kind").and_then(|v| v.as_str()).map(str::to_string)
                    }
                    _ => None,
                };
                let is_turn = request_kind.as_deref() == Some(crate::turn::TURN_KIND);
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
                }
                // Silently ignore when not configured.
            }

            EffectKind::EmitInternal => {
                self.internal.execute(effect, sink, obs).await;
            }

            EffectKind::InstantiateSubmachine => {
                tracing::warn!("CompositeExecutor: InstantiateSubmachine not yet wired; ignored");
            }
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builds a [`CompositeExecutor`] by composing sub-executors incrementally.
#[derive(Default)]
pub struct CompositeExecutorBuilder {
    turn: Option<TurnExecutor>,
    tool: Option<ToolExecutor>,
    user: Option<UserExecutor>,
    timer: Option<TimerExecutor>,
    checkpoint: Option<CheckpointExecutor>,
    audit: Option<AuditExecutor>,
}

impl CompositeExecutorBuilder {
    /// Attach the single-turn engine (kernel-native loop).
    ///
    /// `CallLlm` effects with `kind="turn"` are handled by `TurnExecutor`,
    /// which streams a single model response, appends to the
    /// `ConversationStore`, and posts `LlmTurnComplete`.
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
