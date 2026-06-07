// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Production [`ChildSpawner`] for the SDLC deliberation engine.
//!
//! [`SdlcChildSpawner`] services `Effect::InstantiateSubmachine` (emitted by
//! [`SdlcMachine`](sven_core::SdlcMachine) when an approved plan decomposes into
//! independent tasks). For each task it builds a **fully isolated** child
//! kernel: a fresh [`Context`], a fresh [`DeliberationExecutor`] (hence a fresh
//! append-only conversation store), running a one-shot
//! [`TaskMachine`](sven_core::TaskMachine). The children run concurrently on
//! their own tokio tasks; when one terminates, its structured result is posted
//! back to the parent as `Event::Internal(SubmachineCompleted { result })` so
//! the parent can aggregate it (append-only) on the execution thread.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sven_config::Config;
use sven_core::TaskMachine;
use sven_executors::{CompositeExecutorBuilder, DeliberationExecutor};
use sven_hsm::{
    event::InternalEvent, ChildSpawner, Context, Event, EventSink, Hsm, MachineId,
    PermissionPolicy, Runtime, SystemClock,
};
use sven_tools::ToolRegistry;

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

    /// Permissive policy for a child: the kernel gates capabilities per state,
    /// but the deliberation loop runs tools *internally*, so the only kernel
    /// effect a child emits is the (capability-free) deliberation `CallLlm`.
    fn child_policy() -> PermissionPolicy {
        use sven_hsm::ToolCapability::{ExecuteShell, GitOperation, NetworkAccess, ReadFile, WriteFile};
        PermissionPolicy::builder()
            .allow_globally([ReadFile, WriteFile, GitOperation, ExecuteShell, NetworkAccess])
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

        // Fresh per-child deliberation executor → isolated conversation store.
        let resolver_config = Arc::clone(&self.config);
        let model_resolver: sven_core::ModelResolver = Arc::new(move |model_str: &str| {
            let model_cfg = sven_model::resolve_model_from_config(&resolver_config, model_str);
            let provider = sven_model::from_config(&model_cfg)?;
            Ok(Arc::from(provider) as Arc<dyn sven_model::ModelProvider>)
        });
        // Each child gets its own cancel slot so siblings never clobber each other.
        let cancel_handle = Arc::new(tokio::sync::Mutex::new(None));
        let deliberation = DeliberationExecutor::new(
            self.default_model.clone(),
            Some(model_resolver),
            Arc::clone(&self.tool_registry),
            cancel_handle,
        );
        let executor = CompositeExecutorBuilder::default()
            .with_deliberation(deliberation)
            .with_tools(Arc::clone(&self.tool_registry), Default::default())
            .with_timers(Arc::new(SystemClock::new()))
            .build();

        let rt = Runtime::spawn(
            Hsm::new(TaskMachine::new(task)),
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
