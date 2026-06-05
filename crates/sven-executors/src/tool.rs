//! Tool effect executor.
//!
//! Handles [`Effect::CallTool`]: performs a second-line capability check,
//! invokes the tool registry, and emits [`Event::ToolSucceeded`] or
//! [`Event::ToolFailed`].
//!
//! The kernel's `validate_effects_are_allowed` is the primary permission gate;
//! this executor performs a lightweight redundant check as a defence-in-depth
//! measure (it cannot check per-state policy or approval state since neither
//! are available at execution time — it only rejects capabilities that are not
//! in the globally-allowed set).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink, ToolCapability};
use sven_tools::{ToolCall, ToolRegistry};

/// Executes [`Effect::CallTool`] using the injected [`ToolRegistry`].
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    /// Globally-allowed capabilities (second-line defence check).
    allowed_capabilities: HashSet<ToolCapability>,
}

impl ToolExecutor {
    /// Creates an executor that always executes tools from `registry`.
    ///
    /// Pass an empty `allowed_capabilities` to skip the redundant check
    /// (relying solely on the kernel's primary gate).
    pub fn new(registry: Arc<ToolRegistry>, allowed_capabilities: HashSet<ToolCapability>) -> Self {
        Self {
            registry,
            allowed_capabilities,
        }
    }

    /// Creates an executor with no additional capability restrictions.
    pub fn unrestricted(registry: Arc<ToolRegistry>) -> Self {
        Self::new(registry, HashSet::new())
    }
}

#[async_trait]
impl EffectExecutor for ToolExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let Effect::CallTool {
            call_id,
            name,
            capability,
            args,
        } = effect
        else {
            return;
        };

        // Second-line capability check (defence-in-depth).
        if !self.allowed_capabilities.is_empty() && !self.allowed_capabilities.contains(&capability)
        {
            tracing::warn!(
                tool = %name,
                ?capability,
                "ToolExecutor: capability not in allow-list (kernel gate should have caught this)"
            );
            let _ = sink
                .emit(Event::ToolFailed {
                    call_id,
                    error: format!(
                        "capability {capability:?} is not permitted in the current context"
                    ),
                })
                .await;
            return;
        }

        let tool_call = ToolCall {
            id: call_id.as_uuid().to_string(),
            name: name.clone(),
            args,
        };

        tracing::debug!(tool = %name, "ToolExecutor: invoking tool");
        let output = self.registry.execute(&tool_call).await;

        if output.is_error {
            let _ = sink
                .emit(Event::ToolFailed {
                    call_id,
                    error: output.content,
                })
                .await;
        } else {
            let observation = serde_json::Value::String(output.content);
            let _ = sink
                .emit(Event::ToolSucceeded {
                    call_id,
                    observation,
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime, ToolCallId, ToolCapability,
    };
    use sven_tools::ToolRegistry;

    use super::ToolExecutor;

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
                        Reaction::Handled(vec![])
                    } else {
                        ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                        Reaction::Transition {
                            target: TS::Done,
                            effects: vec![],
                            rationale: "got domain event".into(),
                        }
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
    }

    async fn run_tool_effect(exec: &mut ToolExecutor, effect: Effect) -> String {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
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
    async fn unknown_tool_emits_tool_failed() {
        let registry = Arc::new(ToolRegistry::new());
        let mut exec = ToolExecutor::unrestricted(registry);

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "nonexistent_tool_xyz".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolFailed");
    }

    #[tokio::test]
    async fn capability_not_in_allow_list_emits_tool_failed() {
        let registry = Arc::new(ToolRegistry::new());
        // Only allow ReadFile; trying to use ExecuteShell should fail.
        let allowed: HashSet<ToolCapability> = [ToolCapability::ReadFile].into_iter().collect();
        let mut exec = ToolExecutor::new(registry, allowed);

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "shell".into(),
            capability: ToolCapability::ExecuteShell,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolFailed");
    }
}
