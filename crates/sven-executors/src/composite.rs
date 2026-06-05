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
//!     .with_llm(Arc::new(DefaultLlmAdapter::new(provider)))
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
use sven_llm::LlmAdapter;
use sven_tools::ToolRegistry;
use tokio::sync::mpsc;

use crate::audit::AuditExecutor;
use crate::checkpoint::CheckpointExecutor;
use crate::converse::ConverseExecutor;
use crate::internal::InternalExecutor;
use crate::llm::LlmExecutor;
use crate::timer::TimerExecutor;
use crate::tool::ToolExecutor;
use crate::user::{ApprovalRequest, UserExecutor, UserQuestion};

/// All sub-executors collected into one structure.
pub struct CompositeExecutor {
    /// Converse turn engine (reactive agent). When present it handles
    /// `CallLlm` effects whose request `kind` is `"converse"`; all other
    /// `CallLlm` requests fall through to [`llm`](Self::llm).
    converse: Option<ConverseExecutor>,
    llm: Option<LlmExecutor>,
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
                let is_converse = matches!(
                    &effect,
                    Effect::CallLlm { request }
                        if request.get("kind").and_then(|v| v.as_str())
                            == Some(crate::converse::CONVERSE_KIND)
                );
                if is_converse {
                    if let Some(exec) = &mut self.converse {
                        exec.execute(effect, sink, obs).await;
                    } else {
                        tracing::warn!(
                            "CompositeExecutor: no Converse executor configured; dropping converse CallLlm"
                        );
                    }
                } else if let Some(exec) = &mut self.llm {
                    exec.execute(effect, sink, obs).await;
                } else {
                    tracing::warn!(
                        "CompositeExecutor: no LLM executor configured; dropping CallLlm"
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
    llm: Option<LlmExecutor>,
    tool: Option<ToolExecutor>,
    user: Option<UserExecutor>,
    timer: Option<TimerExecutor>,
    checkpoint: Option<CheckpointExecutor>,
    audit: Option<AuditExecutor>,
}

impl CompositeExecutorBuilder {
    /// Attach the LLM executor backed by `adapter`.
    pub fn with_llm(mut self, adapter: Arc<dyn LlmAdapter>) -> Self {
        self.llm = Some(LlmExecutor::new(adapter));
        self
    }

    /// Attach the converse turn engine backed by a shared [`sven_core::Agent`].
    ///
    /// When present, `CallLlm` effects whose request `kind` is `"converse"`
    /// (emitted by `ReactiveAgentMachine`) are driven through the full legacy
    /// agentic loop with streaming observations.
    pub fn with_converse(mut self, agent: Arc<tokio::sync::Mutex<sven_core::Agent>>) -> Self {
        self.converse = Some(ConverseExecutor::new(agent));
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
            llm: self.llm,
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
    use sven_llm::MockLlmAdapter;
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
            super::CompositeExecutor::builder().build(), // NoOp for the runtime's own executor
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
    async fn composite_routes_call_llm_to_llm_executor() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![Event::LlmProposedAssessment {
            assessment: json!({"intent": "bugfix", "confidence": 0.9}),
        }]));
        let (q_tx, _q_rx) = tokio::sync::mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = tokio::sync::mpsc::channel::<ApprovalRequest>(4);
        let mut exec = CompositeExecutorBuilder::default()
            .with_llm(adapter)
            .with_user(q_tx, a_tx)
            .with_tools(Arc::new(ToolRegistry::new()), Default::default())
            .build();

        let req = sven_llm::LlmRequest::ExtractIntent {
            text: "fix it".into(),
            allowed_intents: vec!["bugfix".into()],
        };
        let effect = Effect::CallLlm {
            request: req.to_value(),
        };
        let kind = run_composite_effect(&mut exec, effect).await;
        assert_eq!(kind, "LlmProposedAssessment");
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
        // Without an LLM executor, the machine never receives an event and
        // stays in Idle. We assert Done=false after a short delay.
        let mut exec = CompositeExecutorBuilder::default().build();

        let req = sven_llm::LlmRequest::ExtractConstraints {
            known_context: json!({}),
        };
        let effect = Effect::CallLlm {
            request: req.to_value(),
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
}
