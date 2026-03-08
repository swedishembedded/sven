// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Constructs `sven_core::Agent` instances used by the node.
//!
//! Two entry points:
//! - [`build_node_agent`] — the long-lived interactive agent that powers
//!   the HTTP, WebSocket and CLI control surfaces.
//! - [`build_task_agent`] — a fresh, per-task agent used exclusively for
//!   executing inbound P2P delegated tasks.  Each call produces a completely
//!   independent agent; there is no shared mutable state between concurrent
//!   inbound tasks.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::Config;
use sven_core::{Agent, AgentRuntimeContext};
use sven_p2p::{protocol::types::AgentCard, P2pHandle};
use sven_team::{
    AssignTaskTool, ClaimTaskTool, CleanupTeamTool, CompleteTaskTool, CreateTaskTool,
    CreateTeamTool, ListTasksTool, ListTeamTool, RegisterTeammateTool, ShutdownTeammateTool,
    SpawnTeammateTool, TaskStoreHandle, TeamConfigHandle, UpdateTaskTool,
};
use sven_tools::{
    DeleteFileTool, EditFileTool, FindFileTool, GrepTool, ListDirTool, ReadFileTool, ReadLintsTool,
    RunTerminalCommandTool, SwitchModeTool, TodoItem, TodoWriteTool, ToolEvent, ToolRegistry,
    UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
};

use crate::tools::{
    BroadcastAbortTool, DelegateTool, DelegationContext, DelegationContextHandle,
    ListConversationsTool, ListPeersTool, PostToRoomTool, ReadRoomHistoryTool, RoomDepthHandle,
    RoomDepthTracker, SearchConversationTool, SendMessageTool, SessionDepthHandle,
    SessionDepthTracker, WaitForMessageTool,
};

/// Optional team context for agents participating in a team.
#[derive(Clone, Default)]
pub struct TeamContext {
    /// Name of the active team (if any).
    pub team_name: Option<String>,
    /// Shared task store handle (shared across all tools in this agent).
    pub task_store: Option<TaskStoreHandle>,
    /// In-memory team config handle.
    pub team_config: TeamConfigHandle,
    /// Name of this agent (for task attribution).
    pub agent_name: String,
    /// Peer ID of this agent (for team lead check).
    pub agent_peer_id: String,
}

/// Build the shared, long-lived node `Agent`.
///
/// `model` must be the pre-constructed model provider.  Passing a shared
/// `Arc` avoids creating a second HTTP client / API connection when
/// `build_task_agent` is later called for inbound P2P tasks.
///
/// The delegation context slot starts empty (`None`); the interactive agent
/// never runs inside a delegated task so it never needs delegation guards.
/// Returns both the built `Agent` and the `SessionDepthHandle` so the caller
/// can reset the per-peer depth map at the start of each new user turn.
///
/// Task agents never need this: they are created fresh per task and discarded
/// when the task completes.  The interactive node agent, however, is
/// long-lived, so each new user session must call
/// `depth.lock().await.reset_per_turn()` before the agent runs.
pub async fn build_node_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
) -> anyhow::Result<(Agent, SessionDepthHandle)> {
    let max_ctx = model
        .config_context_window()
        .or_else(|| model.catalog_context_window())
        .unwrap_or(128_000) as usize;

    // Empty delegation context — the interactive agent is never itself
    // executing inside a delegated task.
    let delegation_context: DelegationContextHandle = Arc::new(Mutex::new(None));

    // Per-peer session depth tracker; default_depth=0 so every new peer
    // conversation starts at depth 0.  The per-peer map ensures that a prior
    // deep conversation with peer B does not pollute a fresh exchange with C.
    // The handle is returned so the caller can reset per_peer at each turn.
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: 0,
        per_peer: HashMap::new(),
    }));

    // Room depth starts at 0 — the node never executes inside a reactive
    // room handler.  default_depth=0 means PostToRoomTool treats all posts as
    // independent topic posts (depth=1) unless the LLM provides in_reply_to_depth.
    let room_depth: RoomDepthHandle = Arc::new(tokio::sync::Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));

    let agent = build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        Arc::clone(&session_depth),
        room_depth,
        AgentRuntimeContext::default(),
    )
    .await?;

    Ok((agent, session_depth))
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
    let max_ctx = model
        .config_context_window()
        .or_else(|| model.catalog_context_window())
        .unwrap_or(128_000) as usize;

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

    // Seed session depth from task_depth so that any session conversation
    // started by this task agent continues the unified hop budget.  Without
    // this, a task at depth 2 would start session chains at depth 1, allowing
    // combined traversals far beyond MAX_HOP_DEPTH.
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: task_depth,
        per_peer: HashMap::new(),
    }));

    // Room depth: task agents can post to rooms but are never reactive handlers.
    // default_depth=0 so independent room posts carry depth=1 and the per_room
    // map is only populated when the LLM explicitly uses in_reply_to_depth.
    let room_depth: RoomDepthHandle = Arc::new(tokio::sync::Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));

    build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        runtime,
    )
    .await
}

/// Build a fresh per-task agent with a fully custom [`AgentRuntimeContext`].
///
/// Used by the session executor so it can inject `prior_messages` and a
/// session-specific system prompt while still reusing the standard tool set.
///
/// `initial_session_depth` is the depth of the inbound `SessionMessageWire`
/// that triggered this agent.  It is used to seed the `SessionDepthHandle`
/// shared between `SendMessageTool` and `WaitForMessageTool`, so that the
/// first explicit `send_message` call carries `initial_session_depth + 1` on
/// the wire and subsequent calls continue incrementing from wherever
/// `wait_for_message` last left off.  Pass `0` for task agents and node
/// interactive sessions.
#[allow(clippy::too_many_arguments)]
pub async fn build_task_agent_with_runtime(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    task_depth: u32,
    task_chain: Vec<String>,
    initial_session_depth: u32,
    runtime: AgentRuntimeContext,
) -> anyhow::Result<Agent> {
    let max_ctx = model
        .config_context_window()
        .or_else(|| model.catalog_context_window())
        .unwrap_or(128_000) as usize;
    let delegation_context: DelegationContextHandle =
        Arc::new(Mutex::new(Some(DelegationContext {
            depth: task_depth,
            chain: task_chain,
        })));
    // `initial_session_depth` is the depth of the inbound message that spawned
    // this agent (session message depth for session agents; 0 for others).
    // Using it as `default_depth` propagates the cross-protocol hop budget:
    // a session agent at depth 3 that starts fresh peer conversations continues
    // from depth 3, not from 0.
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: initial_session_depth,
        per_peer: HashMap::new(),
    }));
    let room_depth: RoomDepthHandle = Arc::new(tokio::sync::Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));
    build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        runtime,
    )
    .await
}

/// Build a fresh agent for the reactive room post executor.
///
/// Seeds `room_depth.default_depth` with `inbound_post_depth` so that any
/// call to `PostToRoomTool` inside the spawned agent automatically carries
/// `inbound_post_depth + 1` as the outgoing depth, correctly propagating the
/// reactive reply chain without requiring the LLM to specify `in_reply_to_depth`
/// for its first response.
///
/// Also seeds `task_depth = inbound_post_depth` and
/// `initial_session_depth = inbound_post_depth` so that any task delegation
/// or session message sent by the reactive agent contributes to the same
/// unified hop budget.
#[allow(clippy::too_many_arguments)]
pub async fn build_room_reactive_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    inbound_post_depth: u32,
    runtime: AgentRuntimeContext,
) -> anyhow::Result<Agent> {
    let max_ctx = model
        .config_context_window()
        .or_else(|| model.catalog_context_window())
        .unwrap_or(128_000) as usize;

    let delegation_context: DelegationContextHandle =
        Arc::new(tokio::sync::Mutex::new(Some(DelegationContext {
            depth: inbound_post_depth,
            chain: vec![],
        })));

    let session_depth: SessionDepthHandle =
        Arc::new(tokio::sync::Mutex::new(SessionDepthTracker {
            default_depth: inbound_post_depth,
            per_peer: HashMap::new(),
        }));

    // Seed default_depth from the inbound post so PostToRoomTool computes
    // outgoing = inbound_post_depth + 1 for the reactive agent's first post.
    let room_depth: RoomDepthHandle = Arc::new(tokio::sync::Mutex::new(RoomDepthTracker {
        default_depth: inbound_post_depth,
        per_room: HashMap::new(),
    }));

    build_agent_with(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        runtime,
    )
    .await
}

/// Shared internal builder used by both [`build_node_agent`] and
/// [`build_task_agent`].
#[allow(clippy::too_many_arguments)]
async fn build_agent_with(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    max_ctx: usize,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    delegation_context: DelegationContextHandle,
    session_depth_handle: SessionDepthHandle,
    room_depth_handle: RoomDepthHandle,
    runtime: AgentRuntimeContext,
) -> anyhow::Result<Agent> {
    build_agent_with_team(
        config,
        model,
        max_ctx,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth_handle,
        room_depth_handle,
        runtime,
        TeamContext::default(),
    )
    .await
}

/// Full internal builder with optional team context.
#[allow(clippy::too_many_arguments)]
async fn build_agent_with_team(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    max_ctx: usize,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    delegation_context: DelegationContextHandle,
    session_depth_handle: SessionDepthHandle,
    room_depth_handle: RoomDepthHandle,
    runtime: AgentRuntimeContext,
    team_ctx: TeamContext,
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
    registry.register(SwitchModeTool::new(mode.clone(), tool_tx.clone()));

    // P2P routing tools.
    registry.register(ListPeersTool {
        p2p: p2p_handle.clone(),
        rooms: rooms.clone(),
    });
    registry.register(DelegateTool {
        p2p: p2p_handle.clone(),
        rooms,
        our_card: agent_card,
        delegation_context,
        tool_tx: Some(tool_tx),
    });

    // Session and room collaboration tools.
    let store = p2p_handle.store().clone();
    registry.register(SendMessageTool {
        p2p: p2p_handle.clone(),
        session_depth: Arc::clone(&session_depth_handle),
    });
    registry.register(WaitForMessageTool {
        p2p: p2p_handle.clone(),
        session_depth: Arc::clone(&session_depth_handle),
    });
    registry.register(SearchConversationTool {
        store: Arc::clone(&store),
    });
    registry.register(ListConversationsTool {
        store: Arc::clone(&store),
    });
    registry.register(PostToRoomTool {
        p2p: p2p_handle.clone(),
        room_depth: room_depth_handle,
    });
    registry.register(ReadRoomHistoryTool {
        store: Arc::clone(&store),
    });

    // Team tools — only registered when a team context is active.
    if let Some(task_store) = team_ctx.task_store {
        registry.register(CreateTaskTool {
            store: task_store.clone(),
            agent_name: team_ctx.agent_name.clone(),
        });
        registry.register(ClaimTaskTool {
            store: task_store.clone(),
            agent_name: team_ctx.agent_name.clone(),
        });
        registry.register(CompleteTaskTool {
            store: task_store.clone(),
        });
        registry.register(ListTasksTool {
            store: task_store.clone(),
        });
        registry.register(AssignTaskTool {
            store: task_store.clone(),
        });
        registry.register(UpdateTaskTool {
            store: task_store.clone(),
        });
    }

    {
        let cfg_handle = team_ctx.team_config.clone();
        let agent_peer_id = team_ctx.agent_peer_id.clone();
        let agent_name_str = team_ctx.agent_name.clone();

        registry.register(CreateTeamTool {
            team_config: cfg_handle.clone(),
            agent_peer_id: agent_peer_id.clone(),
            agent_name: agent_name_str.clone(),
        });
        registry.register(ListTeamTool {
            config: cfg_handle.clone(),
        });
        registry.register(CleanupTeamTool {
            config: cfg_handle.clone(),
            agent_peer_id: agent_peer_id.clone(),
        });
        registry.register(RegisterTeammateTool {
            config: cfg_handle.clone(),
        });
        registry.register(SpawnTeammateTool {
            config: cfg_handle.clone(),
            agent_peer_id: agent_peer_id.clone(),
            sven_bin: None,
            use_worktree: false,
        });
        registry.register(ShutdownTeammateTool {
            config: cfg_handle.clone(),
            agent_peer_id: agent_peer_id.clone(),
        });
        registry.register(BroadcastAbortTool {
            p2p: p2p_handle.clone(),
            agent_peer_id,
            team_config: cfg_handle,
        });
    }

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
