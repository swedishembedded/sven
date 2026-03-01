// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Constructs `sven_core::Agent` instances used by the gateway.
//!
//! Two entry points:
//! - [`build_gateway_agent`] — the long-lived interactive agent that powers
//!   the HTTP, WebSocket and CLI control surfaces.
//! - [`build_task_agent`] — a fresh, per-task agent used exclusively for
//!   executing inbound P2P delegated tasks.  Each call produces a completely
//!   independent agent; there is no shared mutable state between concurrent
//!   inbound tasks.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::Config;
use sven_core::{Agent, AgentRuntimeContext};
use sven_p2p::{protocol::types::AgentCard, P2pHandle};
use sven_tools::{
    DeleteFileTool, EditFileTool, FindFileTool, GrepTool, ListDirTool, ReadFileTool, ReadLintsTool,
    RunTerminalCommandTool, SwitchModeTool, TodoItem, TodoWriteTool, ToolEvent, ToolRegistry,
    UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
};

use crate::tools::{DelegateTool, DelegationContext, DelegationContextHandle, ListPeersTool};

/// Build the shared, long-lived gateway `Agent`.
///
/// `model` must be the pre-constructed model provider.  Passing a shared
/// `Arc` avoids creating a second HTTP client / API connection when
/// `build_task_agent` is later called for inbound P2P tasks.
///
/// The delegation context slot starts empty (`None`); the interactive agent
/// never runs inside a delegated task so it never needs delegation guards.
pub async fn build_gateway_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
) -> anyhow::Result<Agent> {
    let max_ctx = model.catalog_context_window().unwrap_or(128_000) as usize;

    // Empty delegation context — the interactive agent is never itself
    // executing inside a delegated task.
    let delegation_context: DelegationContextHandle = Arc::new(Mutex::new(None));

    build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        AgentRuntimeContext::default(),
    )
    .await
}

/// Build a fresh, **per-task** agent for executing an inbound P2P task.
///
/// Every inbound task gets its own completely isolated agent instance —
/// independent tool registry, delegation context, and session history.
/// This guarantees that concurrent inbound tasks never interfere with each
/// other's delegation depth / chain tracking.
///
/// `task_depth` and `task_chain` are taken directly from the inbound
/// [`TaskRequest`] wire fields and baked into the agent's `DelegateTool` at
/// construction time, so they can never be corrupted by another concurrent
/// task.
///
/// The P2P delegation guidelines are injected into the system prompt via
/// `append_system_prompt` so the LLM immediately understands that it must
/// execute the task locally and may only delegate as a last resort.
pub async fn build_task_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    task_depth: u32,
    task_chain: Vec<String>,
) -> anyhow::Result<Agent> {
    let max_ctx = model.catalog_context_window().unwrap_or(128_000) as usize;

    // Pre-populate the delegation context with this task's depth and chain.
    // No other task will ever touch this Arc — it is created fresh here.
    let delegation_context: DelegationContextHandle =
        Arc::new(Mutex::new(Some(DelegationContext {
            depth: task_depth,
            chain: task_chain,
        })));

    // Inject the P2P execution directive into the system prompt so the LLM
    // understands from the first token that it must attempt the task locally.
    let runtime = AgentRuntimeContext {
        append_system_prompt: Some(sven_core::prompts::p2p_task_guidelines().to_string()),
        ..AgentRuntimeContext::default()
    };

    build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        runtime,
    )
    .await
}

/// Shared internal builder used by both [`build_gateway_agent`] and
/// [`build_task_agent`].
async fn build_agent_with(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    max_ctx: usize,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    delegation_context: DelegationContextHandle,
    runtime: AgentRuntimeContext,
) -> anyhow::Result<Agent> {
    let mode = Arc::new(Mutex::new(config.agent.default_mode));
    let (tool_tx, tool_rx) = mpsc::channel::<ToolEvent>(64);
    let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));

    let mut registry = ToolRegistry::new();

    // Standard tools (same set as the TUI/CI runner).
    registry.register(RunTerminalCommandTool::default());
    registry.register(ReadFileTool);
    registry.register(WriteTool);
    registry.register(EditFileTool);
    registry.register(FindFileTool);
    registry.register(GrepTool);
    registry.register(ListDirTool);
    registry.register(DeleteFileTool);
    registry.register(WebFetchTool);
    registry.register(WebSearchTool {
        api_key: config.tools.web.search.api_key.clone(),
    });
    registry.register(ReadLintsTool);
    registry.register(UpdateMemoryTool {
        memory_file: config.tools.memory.memory_file.clone(),
    });
    registry.register(TodoWriteTool::new(todos, tool_tx.clone()));
    registry.register(SwitchModeTool::new(mode.clone(), tool_tx));

    // P2P routing tools.
    registry.register(ListPeersTool {
        p2p: p2p_handle.clone(),
        rooms: rooms.clone(),
    });
    registry.register(DelegateTool {
        p2p: p2p_handle,
        rooms,
        our_card: agent_card,
        delegation_context,
    });

    Ok(Agent::new(
        model,
        Arc::new(registry),
        Arc::new(config.agent.clone()),
        runtime,
        mode,
        tool_rx,
        max_ctx,
    ))
}
