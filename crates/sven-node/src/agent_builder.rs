// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Constructs the `sven_core::Agent` used by the gateway.
//!
//! Registers the full standard toolset **plus** gateway-specific P2P tools
//! (`delegate_task`, `list_peers`) when a `P2pHandle` is provided.

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

use crate::tools::{DelegateTool, ListPeersTool};

/// Build the gateway `Agent` with all standard tools plus P2P routing tools.
///
/// The `p2p_handle` and `agent_card` are used to register `delegate_task` and
/// `list_peers`.  Both tools are safe to register even before peers connect —
/// they simply return "no peers connected" if the roster is empty.
pub async fn build_gateway_agent(
    config: &Arc<Config>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
) -> anyhow::Result<Agent> {
    let model: Arc<dyn sven_model::ModelProvider> =
        Arc::from(sven_model::from_config(&config.model)?);
    let max_ctx = model.catalog_context_window().unwrap_or(128_000) as usize;

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

    // P2P routing tools — only available when the gateway is running.
    registry.register(ListPeersTool {
        p2p: p2p_handle.clone(),
        rooms: rooms.clone(),
    });
    registry.register(DelegateTool {
        p2p: p2p_handle,
        rooms,
        our_card: agent_card,
    });

    let runtime = AgentRuntimeContext::default();

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
