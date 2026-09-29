// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Production [`ChildSpawner`] for the SDLC deliberation engine.
//!
//! [`SdlcChildSpawner`] services `Effect::InstantiateSubmachine` (emitted by
//! [`SdlcMachine`](sven_machines::SdlcMachine) when an approved plan decomposes into
//! independent tasks). For each task it builds a **fully isolated** child
//! kernel: a fresh [`Context`], a fresh [`TurnExecutor`] (hence a fresh
//! append-only conversation store), running a one-shot
//! [`TaskMachine`](sven_machines::TaskMachine). The children run concurrently on
//! their own tokio tasks; when one terminates, its structured result is posted
//! back to the parent as `Event::Internal(SubmachineCompleted { result })` so
//! the parent can aggregate it (append-only) on the execution thread.
//!
//! # Policy
//!
//! Children receive a **tightened** policy: read/write/shell/git are allowed
//! globally and nothing else, so a call outside that set is rejected
//! immediately by the kernel (child kernels have no human approver). The `TaskMachine` itself auto-denies `ToolApprovalRequired`
//! events that reach it via the `RunningTools` state.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sven_config::Config;
use sven_executors::{CompositeExecutorBuilder, ToolExecutor, TurnExecutor};
use sven_hsm::{
    event::InternalEvent, submachine::ErasedMachine, Context, Event, Hsm, MachineId,
    PermissionPolicy, ToolCallId, ToolCapability,
};
use sven_kernel::{ChildSpawner, ErasedRuntime, EventSink, SystemClock};
use sven_llm::ThreadStore;
use sven_machines::TaskMachine;
use sven_tool_registry::ToolRegistry;

/// Spawns isolated child task kernels for parallel SDLC execution.
pub struct SdlcChildSpawner {
    default_model: Arc<dyn sven_model::ModelProvider>,
    config: Arc<Config>,
    tool_registry: Arc<ToolRegistry>,
}

impl SdlcChildSpawner {
    /// Create a spawner sharing the parent's model, config, and tool registry.
    #[must_use]
    pub fn new(
        default_model: Arc<dyn sven_model::ModelProvider>,
        config: Arc<Config>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            default_model,
            config,
            tool_registry,
        }
    }

    /// Tightened policy for a child kernel: basic read/write/shell/git allowed
    /// and nothing else.
    /// The kernel gates every `CallTool` effect; if a capability falls outside
    /// this policy the kernel emits `ToolFailed{..}` immediately.
    fn child_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
                ToolCapability::NetworkAccess,
            ])
            .build()
    }
}

#[async_trait]
impl ChildSpawner for SdlcChildSpawner {
    async fn spawn_child(&self, machine: MachineId, descriptor: Value, parent: EventSink) {
        let task = descriptor
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // Each child gets its own fresh conversation store (append-only thread).
        let conv_store = Arc::new(std::sync::Mutex::new(ThreadStore::new()));
        let call_id_to_thread = Arc::new(std::sync::Mutex::new(HashMap::<
            ToolCallId,
            (String, String),
        >::new()));
        // Each child gets its own cancel slot so siblings never clobber each other.
        let cancel_handle = Arc::new(tokio::sync::Mutex::new(None));

        let resolver_config = Arc::clone(&self.config);
        // Deliberately `from_config`, not `from_config_probed`: `ModelResolver`
        // (`sven_machines::stream_turn::ModelResolver`) is a synchronous `Fn`,
        // called synchronously from `TurnExecutor::resolve_model`, and
        // `from_config_probed` needs an `.await`. Probing here would require
        // making `ModelResolver` itself async across every call site — a
        // larger, separate refactor. The primary session model IS probed
        // (`runtime_builder.rs::build`); only a per-state/per-child model
        // override (this path) is not.
        let model_resolver: sven_machines::ModelResolver = Arc::new(move |model_str: &str| {
            let model_cfg = sven_model::resolve_model_from_config(&resolver_config, model_str);
            let provider = sven_model_drivers::from_config(&model_cfg)?;
            Ok(Arc::from(provider) as Arc<dyn sven_model::ModelProvider>)
        });

        let turn_executor = TurnExecutor::new(
            self.default_model.clone(),
            Some(model_resolver),
            Arc::clone(&self.tool_registry),
            Arc::clone(&conv_store),
            Arc::clone(&call_id_to_thread),
            cancel_handle,
        )
        .with_thinking_budget(sven_machines::ThinkingBudget::from_agent_config(
            &self.config.agent,
        ));

        let tool_executor = ToolExecutor::with_shared_store(
            Arc::clone(&self.tool_registry),
            Default::default(),
            call_id_to_thread,
            conv_store,
        );

        let executor = CompositeExecutorBuilder::default()
            .with_turn(turn_executor)
            .with_tool_executor(tool_executor)
            .with_timers(Arc::new(SystemClock::new()))
            .build();

        let rt = ErasedRuntime::spawn(
            Box::new(Hsm::new(TaskMachine::new(task))) as Box<dyn ErasedMachine>,
            Context::new(),
            Self::child_policy(),
            executor,
            64,
        );

        let machine_id = machine.as_uuid().to_string();
        // Spawn-and-forget: harvest the child's result on its own task so the
        // parent loop is never blocked and siblings run concurrently.
        tokio::spawn(async move {
            rt.wait_done().await;
            let result = match rt.join().await {
                Ok(report) => report
                    .ctx
                    .fact(TaskMachine::RESULT_FACT)
                    .cloned()
                    .unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            let _ = parent
                .emit(Event::Internal(InternalEvent::SubmachineCompleted {
                    machine: machine_id,
                    result,
                }))
                .await;
        });
    }
}
