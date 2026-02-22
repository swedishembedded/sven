// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! [`AgentBuilder`] — single entry point for constructing a fully wired Agent.
//!
//! Callers pass a [`Config`], an optional [`RuntimeContext`], the desired
//! mode and model, and a [`ToolSetProfile`].  The builder handles registry
//! construction and [`AgentRuntimeContext`] population internally.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentRuntimeContext};
use sven_model::ModelProvider;
use sven_tools::events::ToolEvent;

use crate::context::{RuntimeContext, ToolSetProfile};
use crate::registry::build_tool_registry;

/// Constructs a fully wired [`Agent`] from configuration and runtime context.
///
/// # Example
/// ```rust,ignore
/// let agent = AgentBuilder::new(config)
///     .with_runtime_context(RuntimeContext::auto_detect())
///     .build(mode, model, ToolSetProfile::Full { ... });
/// ```
pub struct AgentBuilder {
    config: Arc<Config>,
    runtime_ctx: RuntimeContext,
}

impl AgentBuilder {
    /// Create a builder with the given configuration.
    /// Runtime context defaults to empty (no project/git/CI detection).
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            runtime_ctx: RuntimeContext::empty(),
        }
    }

    /// Set the runtime context (project root, git, CI environment).
    pub fn with_runtime_context(mut self, ctx: RuntimeContext) -> Self {
        self.runtime_ctx = ctx;
        self
    }

    /// Build the [`Agent`] with the given mode, model, and tool-set profile.
    ///
    /// This method owns the creation of the shared mode lock and tool-event
    /// channel so that `SwitchModeTool` / `TodoWriteTool` and the agent loop
    /// operate on **the same** instances:
    ///
    /// 1. Creates `mode_lock` (same Arc for both the registry and the Agent).
    /// 2. Creates `(tool_event_tx, tool_event_rx)` (tx → tools, rx → Agent).
    /// 3. Converts [`RuntimeContext`] → [`AgentRuntimeContext`].
    /// 4. Builds a [`ToolRegistry`] via `build_tool_registry`.
    /// 5. Constructs `Agent::new(...)`.
    pub fn build(
        self,
        mode: AgentMode,
        model: Arc<dyn ModelProvider>,
        profile: ToolSetProfile,
    ) -> Agent {
        // Shared mode lock: SwitchModeTool holds a clone; the agent owns it.
        let mode_lock = Arc::new(Mutex::new(mode));
        // Shared event channel: tools send, agent drains.
        let (tool_event_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);

        // Convert RuntimeContext → AgentRuntimeContext (the sven-core type).
        let runtime = AgentRuntimeContext {
            project_root: self.runtime_ctx.project_root,
            git_context_note: self.runtime_ctx.git_context
                .and_then(|g| g.to_prompt_section()),
            ci_context_note: self.runtime_ctx.ci_context
                .and_then(|c| c.to_prompt_section()),
            project_context_file: self.runtime_ctx.project_context_file,
            append_system_prompt: self.runtime_ctx.append_system_prompt,
            system_prompt_override: self.runtime_ctx.system_prompt_override,
        };

        // Pass runtime.clone() as sub_agent_runtime so TaskTool sub-agents
        // inherit the parent's project root, AGENTS.md, CI/git context.
        let registry = build_tool_registry(
            &self.config,
            model.clone(),
            profile,
            mode_lock.clone(),
            tool_event_tx,
            runtime.clone(),
        );

        // Resolve context window from the static catalog; fall back to 128 000.
        let context_window = model.catalog_context_window().unwrap_or(128_000) as usize;

        Agent::new(
            model,
            Arc::new(registry),
            Arc::new(self.config.agent.clone()),
            runtime,
            mode_lock,
            tool_event_rx,
            context_window,
        )
    }
}
