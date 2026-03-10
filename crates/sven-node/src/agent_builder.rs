// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Constructs `sven_core::Agent` instances used by the node.
//!
//! ## Entry points
//!
//! - [`build_node_agent`] — the long-lived interactive agent that powers
//!   the HTTP, WebSocket and CLI control surfaces.
//! - [`build_task_agent`] — a fresh, per-task agent used exclusively for
//!   executing inbound P2P delegated tasks.
//! - [`build_task_agent_with_runtime`] — per-task agent with a custom
//!   [`AgentRuntimeContext`] (used by the session executor).
//! - [`build_room_reactive_agent`] — per-room-post reactive agent.
//!
//! ## Three-layer tool composition
//!
//! ```text
//! Layer 1 (common)   build_tool_registry(profile)    via sven-bootstrap
//! Layer 2 (P2P)      register_p2p_tools()            always
//! Layer 3 (team)     register_team_tools()           main node agent only
//! ```
//!
//! Only the interactive node agent gets Layer 3.  P2P inbound agents
//! (task, session, room) get Layers 1 (SubAgent profile) + 2, preventing
//! recursive team creation inside delegated tasks.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_bootstrap::{build_tool_registry, OutputBufferStore, RuntimeContext, ToolSetProfile};
use sven_config::Config;
use sven_core::{Agent, AgentRuntimeContext};
use sven_p2p::{protocol::types::AgentCard, P2pHandle};
use sven_team::{
    AssignTaskTool, ClaimTaskTool, CleanupTeamTool, CompleteTaskTool, CreateTaskTool,
    CreateTeamTool, ListTasksTool, ListTeamTool, LoadTeamTool, MergeTeammateBranchTool,
    ReadTeammateLogTool, RegisterTeammateTool, ShutdownTeammateTool, SpawnTeammateTool,
    TeamConfigHandle, UpdateTaskTool,
};
use sven_tools::{events::TodoItem, ToolEvent, ToolRegistry};

use crate::tools::{
    BroadcastAbortTool, DelegateTool, DelegationContext, DelegationContextHandle,
    ListConversationsTool, ListPeersTool, PostToRoomTool, ReadRoomHistoryTool, RoomDepthHandle,
    RoomDepthTracker, SearchConversationTool, SendMessageTool, SessionDepthHandle,
    SessionDepthTracker, WaitForMessageTool,
};

// ── TeamContext ────────────────────────────────────────────────────────────────

/// Optional team context for agents participating in a team.
///
/// Holds only the lightweight handles needed to construct the team tools.
/// The `TaskStore` is opened lazily inside each tool's `execute()` call, so no
/// pre-opened handle is required here.
#[derive(Clone, Default)]
pub struct TeamContext {
    /// In-memory team config handle (shared across all team tools).
    pub team_config: TeamConfigHandle,
    /// Name of this agent (for task attribution and team registration).
    pub agent_name: String,
    /// Peer ID of this agent (for team lead checks).
    pub agent_peer_id: String,
}

// ── Public builders ────────────────────────────────────────────────────────────

/// Build the shared, long-lived node [`Agent`].
///
/// Receives `ToolSetProfile::Full` (includes `TaskTool`, buffer tools, skills,
/// knowledge, context, and GDB tools) plus all P2P tools and, when a team is
/// active, all team lifecycle and task-management tools.
///
/// Returns both the built [`Agent`] and the [`SessionDepthHandle`] so the
/// caller can reset the per-peer depth map at the start of each new user turn.
#[allow(clippy::too_many_arguments)]
pub async fn build_node_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    runtime_ctx: &RuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    team_ctx: TeamContext,
) -> anyhow::Result<(Agent, SessionDepthHandle)> {
    let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
    let profile = ToolSetProfile::Full {
        question_tx: None,
        todos,
        buffer_store: Arc::clone(&buffer_store),
    };

    let delegation_context: DelegationContextHandle = Arc::new(Mutex::new(None));
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: 0,
        per_peer: HashMap::new(),
    }));
    let room_depth: RoomDepthHandle = Arc::new(Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));

    let agent_runtime = runtime_ctx.to_agent_runtime();

    let agent = build_node_agent_inner(
        config,
        model,
        profile,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        Arc::clone(&session_depth),
        room_depth,
        agent_runtime,
        Some(team_ctx),
    )
    .await?;

    Ok((agent, session_depth))
}

/// Build a fresh, **per-task** agent for executing an inbound P2P task.
///
/// Receives `ToolSetProfile::SubAgent` (no `TaskTool`, preventing recursive
/// subprocess+P2P nesting) plus all P2P tools.  Team tools are intentionally
/// omitted so inbound task agents cannot create or manage teams.
///
/// `task_depth` and `task_chain` are taken directly from the inbound
/// [`TaskRequest`] wire fields and baked into the agent's `DelegateTool` at
/// construction time.
#[allow(clippy::too_many_arguments)]
pub async fn build_task_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    task_depth: u32,
    task_chain: Vec<String>,
    runtime_ctx: &RuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> anyhow::Result<Agent> {
    let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
    let profile = ToolSetProfile::SubAgent {
        todos,
        buffer_store: Arc::clone(&buffer_store),
    };

    let delegation_context: DelegationContextHandle =
        Arc::new(Mutex::new(Some(DelegationContext {
            depth: task_depth,
            chain: task_chain,
        })));
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: task_depth,
        per_peer: HashMap::new(),
    }));
    let room_depth: RoomDepthHandle = Arc::new(Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));

    let mut agent_runtime = runtime_ctx.to_agent_runtime();
    agent_runtime.append_system_prompt =
        Some(sven_core::prompts::p2p_task_guidelines().to_string());

    build_node_agent_inner(
        config,
        model,
        profile,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        agent_runtime,
        None,
    )
    .await
}

/// Build a fresh per-task agent with a fully custom [`AgentRuntimeContext`].
///
/// Used by the session executor so it can inject `prior_messages` and a
/// session-specific system prompt while still reusing the standard tool set.
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
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> anyhow::Result<Agent> {
    let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
    let profile = ToolSetProfile::SubAgent {
        todos,
        buffer_store: Arc::clone(&buffer_store),
    };

    let delegation_context: DelegationContextHandle =
        Arc::new(Mutex::new(Some(DelegationContext {
            depth: task_depth,
            chain: task_chain,
        })));
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: initial_session_depth,
        per_peer: HashMap::new(),
    }));
    let room_depth: RoomDepthHandle = Arc::new(Mutex::new(RoomDepthTracker {
        default_depth: 0,
        per_room: HashMap::new(),
    }));

    build_node_agent_inner(
        config,
        model,
        profile,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        runtime,
        None,
    )
    .await
}

/// Build a fresh agent for the reactive room post executor.
///
/// Seeds `room_depth.default_depth` with `inbound_post_depth` so that any
/// call to `PostToRoomTool` inside the spawned agent automatically carries
/// `inbound_post_depth + 1` as the outgoing depth.
#[allow(clippy::too_many_arguments)]
pub async fn build_room_reactive_agent(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    inbound_post_depth: u32,
    runtime: AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> anyhow::Result<Agent> {
    let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
    let profile = ToolSetProfile::SubAgent {
        todos,
        buffer_store: Arc::clone(&buffer_store),
    };

    let delegation_context: DelegationContextHandle =
        Arc::new(Mutex::new(Some(DelegationContext {
            depth: inbound_post_depth,
            chain: vec![],
        })));
    let session_depth: SessionDepthHandle = Arc::new(Mutex::new(SessionDepthTracker {
        default_depth: inbound_post_depth,
        per_peer: HashMap::new(),
    }));
    let room_depth: RoomDepthHandle = Arc::new(Mutex::new(RoomDepthTracker {
        default_depth: inbound_post_depth,
        per_room: HashMap::new(),
    }));

    build_node_agent_inner(
        config,
        model,
        profile,
        p2p_handle,
        agent_card,
        rooms,
        delegation_context,
        session_depth,
        room_depth,
        runtime,
        None,
    )
    .await
}

// ── Internal builder ───────────────────────────────────────────────────────────

/// Single internal builder that all public functions delegate to.
///
/// Applies the three-layer tool composition:
///
/// 1. **Layer 1** — common tools via `build_tool_registry(profile)`.
/// 2. **Layer 2** — P2P routing tools (always).
/// 3. **Layer 3** — team lifecycle + task-management tools (only when
///    `team_ctx` is `Some`, i.e. the main interactive node agent).
#[allow(clippy::too_many_arguments)]
async fn build_node_agent_inner(
    config: &Arc<Config>,
    model: Arc<dyn sven_model::ModelProvider>,
    profile: ToolSetProfile,
    p2p_handle: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    delegation_context: DelegationContextHandle,
    session_depth_handle: SessionDepthHandle,
    room_depth_handle: RoomDepthHandle,
    agent_runtime: AgentRuntimeContext,
    team_ctx: Option<TeamContext>,
) -> anyhow::Result<Agent> {
    let mode = Arc::new(Mutex::new(config.agent.default_mode));
    let (tool_tx, tool_rx) = mpsc::channel::<ToolEvent>(64);

    // Layer 1: common tools via sven-bootstrap registry builder.
    let mut registry = build_tool_registry(
        config,
        model.clone(),
        profile,
        tool_tx.clone(),
        agent_runtime.clone(),
    );

    // Layer 2: P2P routing and collaboration tools.
    register_p2p_tools(
        &mut registry,
        p2p_handle.clone(),
        agent_card,
        rooms,
        delegation_context,
        session_depth_handle,
        room_depth_handle,
        tool_tx.clone(),
    );

    // Layer 3: team tools (main node agent only).
    if let Some(ctx) = team_ctx {
        register_team_tools(&mut registry, &ctx, &p2p_handle);
    }

    let max_ctx = model
        .config_context_window()
        .or_else(|| model.catalog_context_window())
        .unwrap_or(128_000) as usize;

    Ok(Agent::new(
        model,
        Arc::new(registry),
        Arc::new(config.agent.clone()),
        agent_runtime,
        mode,
        tool_rx,
        max_ctx,
    ))
}

// ── Layer 2: P2P tools registration ───────────────────────────────────────────

/// Register all P2P routing and collaboration tools into `registry`.
///
/// These are added for every agent variant (interactive node agent, P2P task
/// agents, session agents, and room-reactive agents) because all of them may
/// need to communicate with peers.
#[allow(clippy::too_many_arguments)]
fn register_p2p_tools(
    registry: &mut ToolRegistry,
    p2p: P2pHandle,
    agent_card: AgentCard,
    rooms: Vec<String>,
    delegation_context: DelegationContextHandle,
    session_depth: SessionDepthHandle,
    room_depth: RoomDepthHandle,
    tool_tx: mpsc::Sender<ToolEvent>,
) {
    // Peer discovery and task delegation.
    registry.register(ListPeersTool {
        p2p: p2p.clone(),
        rooms: rooms.clone(),
    });
    registry.register(DelegateTool {
        p2p: p2p.clone(),
        rooms,
        our_card: agent_card,
        delegation_context,
        tool_tx: Some(tool_tx),
    });

    // Session and room collaboration tools.
    let store = p2p.store().clone();
    registry.register(SendMessageTool {
        p2p: p2p.clone(),
        session_depth: Arc::clone(&session_depth),
    });
    registry.register(WaitForMessageTool {
        p2p: p2p.clone(),
        session_depth: Arc::clone(&session_depth),
    });
    registry.register(SearchConversationTool {
        store: Arc::clone(&store),
    });
    registry.register(ListConversationsTool {
        store: Arc::clone(&store),
    });
    registry.register(PostToRoomTool {
        p2p: p2p.clone(),
        room_depth,
    });
    registry.register(ReadRoomHistoryTool {
        store: Arc::clone(&store),
    });
}

// ── Layer 3: team tools registration ──────────────────────────────────────────

/// Register team lifecycle and task-management tools into `registry`.
///
/// Called only for the main interactive node agent.  P2P inbound agents
/// (task, session, room-reactive) do not receive these tools, preventing
/// recursive team creation inside delegated tasks.
///
/// All six task tools open the [`sven_team::TaskStore`] lazily inside their
/// `execute()` calls, so they are always safe to register regardless of
/// whether a team is currently active.
fn register_team_tools(registry: &mut ToolRegistry, ctx: &TeamContext, p2p: &P2pHandle) {
    let cfg = ctx.team_config.clone();
    let agent_name = ctx.agent_name.clone();
    let agent_peer_id = ctx.agent_peer_id.clone();

    // Task management tools — lazy TaskStore, always registered.
    registry.register(CreateTaskTool {
        team_config: cfg.clone(),
        agent_name: agent_name.clone(),
    });
    registry.register(ClaimTaskTool {
        team_config: cfg.clone(),
        agent_name: agent_name.clone(),
    });
    registry.register(CompleteTaskTool {
        team_config: cfg.clone(),
    });
    registry.register(ListTasksTool {
        team_config: cfg.clone(),
    });
    registry.register(AssignTaskTool {
        team_config: cfg.clone(),
    });
    registry.register(UpdateTaskTool {
        team_config: cfg.clone(),
    });

    // Team lifecycle tools.
    registry.register(CreateTeamTool {
        team_config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
        agent_name: agent_name.clone(),
    });
    registry.register(ListTeamTool {
        config: cfg.clone(),
    });
    registry.register(CleanupTeamTool {
        config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
    });
    registry.register(RegisterTeammateTool {
        config: cfg.clone(),
    });
    registry.register(SpawnTeammateTool {
        config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
        sven_bin: None,
        use_worktree: false,
    });
    registry.register(ShutdownTeammateTool {
        config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
    });
    registry.register(MergeTeammateBranchTool {
        config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
    });
    registry.register(LoadTeamTool {
        config: cfg.clone(),
        agent_peer_id: agent_peer_id.clone(),
    });
    registry.register(ReadTeammateLogTool {
        config: cfg.clone(),
    });
    registry.register(BroadcastAbortTool {
        p2p: p2p.clone(),
        agent_peer_id,
        team_config: cfg,
    });
}
