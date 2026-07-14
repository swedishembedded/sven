//! Tool effect executor.
//!
//! Handles [`Effect::CallTool`]: performs a second-line capability check,
//! spawns the tool execution on a dedicated tokio task (spawn-and-forget so a
//! batch of `CallTool` effects runs concurrently), and emits
//! [`Event::ToolSucceeded`] or [`Event::ToolFailed`] when the task completes.
//!
//! ## Concurrency
//!
//! Each `CallTool` effect spawns an independent task — there is no sequential
//! waiting.  The kernel's single consumer loop returns immediately after
//! dispatching all effects, and tool results arrive back as events in whatever
//! order the tasks finish.  This restores the parallel-tools behaviour that was
//! lost when the old agent loop awaited each tool sequentially.
//!
//! ## `call_id → thread` registry
//!
//! An optional [`Arc<Mutex<HashMap<ToolCallId, String>>>`] maps call IDs to
//! conversation thread IDs.  When set and a mapping exists for the completing
//! call, the tool result is also appended to that thread in the shared
//! [`ConversationStore`] (append-only; never mutates prior messages).  This
//! registry is populated by the `TurnExecutor` in Phase B.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_hsm::{
    Effect, EffectExecutor, Event, EventSink, ObservationSink, ToolCallId, ToolCapability, UiEvent,
};
use sven_llm::ConversationStore;
use sven_model::Message;
use sven_tools::{ToolCall, ToolRegistry};

/// Executes [`Effect::CallTool`] using the injected [`ToolRegistry`].
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    /// Globally-allowed capabilities (second-line defence check).
    allowed_capabilities: HashSet<ToolCapability>,
    /// Maps `call_id → (thread_id, original_call_id)`; populated by `TurnExecutor`.
    pub call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    /// Shared conversation store; tool results are appended here when a
    /// thread mapping exists.
    pub store: Arc<Mutex<ConversationStore>>,
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
            call_id_to_thread: Arc::new(Mutex::new(HashMap::new())),
            store: Arc::new(Mutex::new(ConversationStore::new())),
        }
    }

    /// Creates an executor with no additional capability restrictions.
    pub fn unrestricted(registry: Arc<ToolRegistry>) -> Self {
        Self::new(registry, HashSet::new())
    }

    /// Creates an executor with a pre-shared call_id registry and store.
    ///
    /// Used when `TurnExecutor` (Phase B) needs to share its registry with this
    /// executor so that tool results land on the right conversation thread.
    pub fn with_shared_store(
        registry: Arc<ToolRegistry>,
        allowed_capabilities: HashSet<ToolCapability>,
        call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
        store: Arc<Mutex<ConversationStore>>,
    ) -> Self {
        Self {
            registry,
            allowed_capabilities,
            call_id_to_thread,
            store,
        }
    }
}

#[async_trait]
impl EffectExecutor for ToolExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
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

        let registry = Arc::clone(&self.registry);
        let call_id_to_thread = Arc::clone(&self.call_id_to_thread);
        let store = Arc::clone(&self.store);
        let sink = sink.clone();
        let obs = obs.clone();

        // Spawn-and-forget: the task runs concurrently with other effects.
        tokio::spawn(async move {
            // Resolve the (thread, original_call_id) mapping before running the
            // tool.  The original_call_id is the raw string the LLM returned (e.g.
            // "call_HyZJn1bTtqVbmzzS3W4VM4xb") — it must match the id recorded in
            // the preceding assistant message or the API will reject the continuation.
            let mapping = call_id_to_thread
                .lock()
                .ok()
                .and_then(|m| m.get(&call_id).cloned());

            let tool_call = ToolCall {
                id: mapping
                    .as_ref()
                    .map(|(_, orig)| orig.clone())
                    .unwrap_or_else(|| call_id.as_uuid().to_string()),
                name: name.clone(),
                args,
            };

            // Display id used for outward observation correlation: the exact
            // id the LLM assigned (matching the earlier `UiEvent::ToolStarted`),
            // falling back to the internal uuid when no mapping exists.
            let display_id = tool_call.id.clone();

            tracing::debug!(tool = %name, "ToolExecutor: invoking tool (spawned)");
            let output = registry.execute(&tool_call).await;

            // Emit the outward completion observation so headless / UI observers
            // can render the tool result. This mirrors `UiEvent::ToolStarted`
            // (emitted by the turn stream) and carries no inward semantics — the
            // authoritative result still flows via `Event::ToolSucceeded` /
            // `Event::ToolFailed` below.
            obs.emit(UiEvent::ToolFinished {
                call_id: display_id,
                name: name.clone(),
                output: output.content.clone(),
                is_error: output.is_error,
            });

            // Append to conversation thread if a mapping exists.
            if let Some((tid, orig)) = mapping {
                if let Ok(mut s) = store.lock() {
                    let msg = if output.is_error {
                        Message::tool_result(orig, format!("error: {}", output.content))
                    } else {
                        Message::tool_result(orig, &output.content)
                    };
                    s.append(&tid, msg);
                }
            }

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
        });
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
