// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Top-level TUI application state and event loop.

pub(crate) mod agent_conn;
pub(crate) mod agent_events;
pub(crate) mod chat_ops;
pub(crate) mod chat_state;
pub(crate) mod construct;
pub(crate) mod dispatch;
pub(crate) mod dispatch_chat;
pub(crate) mod dispatch_input;
pub(crate) mod hit_test;
pub(crate) mod input_state;
pub(crate) mod layout_cache;
pub(crate) mod nvim_state;
pub(crate) mod queue_state;
pub(crate) mod render;
pub(crate) mod run;
pub(crate) mod session_lifecycle;
pub(crate) mod session_manager;
pub(crate) mod term_events;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod ui_state;

use std::path::PathBuf;
use std::sync::Arc;

use sven_config::{AgentMode, Config};
use sven_mcp_client::McpManager;
use sven_tools_agent::QuestionRequest;
use tokio::sync::mpsc;

use sven_bootstrap::OutputBufferStore;

use crate::chat::segment::ChatSegment;
use crate::commands::{CommandRegistry, CompletionManager};

pub(crate) use agent_conn::AgentConn;
pub(crate) use chat_state::ChatState;
pub(crate) use input_state::{EditState, InputState};
pub(crate) use layout_cache::{LayoutCache, SplitPrefs};
pub(crate) use nvim_state::NvimState;
pub(crate) use queue_state::QueueState;
pub(crate) use session_manager::SessionManager;
pub(crate) use ui_state::UiState;

// Re-export FocusPane at the app module level - imported from `crate::app::FocusPane`
// throughout the codebase.
pub use ui_state::FocusPane;

// ── Public types (re-exported from sven-frontend for cross-frontend sharing) ───

pub use sven_frontend::{ModelDirective, NodeBackend, QueuedMessage};

/// Options passed when constructing the TUI app.
pub struct AppOptions {
    pub mode: AgentMode,
    pub initial_prompt: Option<String>,
    pub initial_history: Option<(Vec<ChatSegment>, PathBuf)>,
    pub no_nvim: bool,
    pub model_override: Option<String>,
    /// Combined load+output ATIF trace path (`--trace`), or the output-only
    /// path (`--output-trace`). Loaded and saved as a native ATIF `Trajectory`
    /// document (`trace_session::load_session_from` /
    /// `atif::persist::write_trajectory_atomic`) - this is now the ONE
    /// session-persistence path for the TUI; there is no separate YAML/JSONL
    /// branch.
    pub trace_path: Option<PathBuf>,
    /// Load-only ATIF trace path (`--load-trace`).
    pub load_trace_path: Option<PathBuf>,
    pub initial_queue: Vec<QueuedMessage>,
    /// When `Some`, connect the TUI to a running node instead of running a
    /// local agent.  Gives the TUI full access to the node's P2P tools.
    pub node_backend: Option<NodeBackend>,
}

// ── App ───────────────────────────────────────────────────────────────────────

/// The top-level TUI application state.
pub struct App {
    // ── Persistent configuration ──────────────────────────────────────────────
    pub(crate) config: Arc<Config>,
    /// Node-proxy backend, consumed once in `run()`.
    pub(crate) node_backend: Option<NodeBackend>,
    /// True when the TUI is connected to a running sven node over WebSocket.
    /// In this mode the node owns model/mode selection; the TUI is a dumb
    /// terminal that only forwards text and renders streamed responses.
    pub(crate) is_node_proxy: bool,
    /// Node URL retained after `run()` consumes `node_backend`, so the
    /// inspector can query the node for its tool list via `/tools`.
    pub(crate) node_url: Option<String>,
    /// Node bearer token, retained alongside `node_url`.
    pub(crate) node_token: Option<String>,
    /// Whether the node connection should skip TLS verification.
    pub(crate) node_insecure: bool,
    pub(crate) session: crate::state::SessionState,
    pub(crate) command_registry: Arc<CommandRegistry>,
    pub(crate) completion_manager: CompletionManager,
    pub(crate) shared_skills: sven_workspace::SharedSkills,
    pub(crate) shared_agents: sven_workspace::SharedAgents,
    /// Shared tool snapshot - populated by the runtime builder after the local
    /// tool registry is built.  Empty in node-proxy mode (tools are fetched live
    /// from the node when `/tools` is opened).
    pub(crate) shared_tools: sven_tools::SharedTools,
    /// MCP manager - populated in local mode after the agent is built.
    /// `None` in node-proxy mode.  Used by `/mcp` to display server status.
    pub(crate) mcp_manager: Option<Arc<McpManager>>,
    /// MCP prompt slash commands, keyed by command name.
    ///
    /// Populated after the McpManager connects and prompts are discovered.
    /// Checked alongside `command_registry` during slash command dispatch.
    pub(crate) mcp_prompt_commands:
        std::collections::HashMap<String, Arc<dyn crate::commands::SlashCommand>>,
    /// Broadcast sender for MCP tool refresh. When ToolsChanged fires, we send
    /// so all agent tasks update their registries.
    pub(crate) mcp_refresh_tx: Option<tokio::sync::broadcast::Sender<()>>,
    /// Tool display registry - set by the runtime builder after the registry is built.
    /// Used for chat view (collapsed summary, display name) when present.
    pub(crate) shared_tool_displays: sven_tools::SharedToolDisplays,
    pub(crate) history_path: Option<PathBuf>,
    /// Set to `true` after a tool call completes - triggers a terminal-state
    /// recovery pass before the next draw.
    pub(crate) needs_terminal_recover: bool,
    /// Shared output buffer store - also held by the agent's `TaskTool` so that
    /// the TUI can display live subprocess buffer status via `/context` or `/peers`.
    pub(crate) buffer_store: Arc<tokio::sync::Mutex<OutputBufferStore>>,

    // ── Grouped sub-state ─────────────────────────────────────────────────────
    pub(crate) chat: ChatState,
    pub(crate) input: InputState,
    pub(crate) edit: EditState,
    pub(crate) queue: QueueState,
    pub(crate) ui: UiState,
    pub(crate) agent: AgentConn,
    pub(crate) nvim: NvimState,
    pub(crate) prefs: SplitPrefs,
    pub(crate) layout: LayoutCache,
    /// Multi-session manager - holds all chat sessions and the shared event mux.
    pub(crate) sessions: SessionManager,
    /// Path to the ATIF trajectory file for the current active session.
    pub(crate) session_path: Option<PathBuf>,
    /// Title of the current active chat session.
    pub(crate) chat_title: String,
    /// Shared question sender - cloned into every agent task so that question
    /// requests from all sessions are routed through the single `question_rx`
    /// in `run()`.  `None` before `run()` is called (e.g. in tests).
    pub(crate) question_tx: Option<mpsc::Sender<QuestionRequest>>,
    /// Sender for toast notifications from background tasks (e.g. OAuth auth).
    /// `None` before `run()` is called.
    pub(crate) toast_tx: Option<mpsc::Sender<ui_state::Toast>>,
}
