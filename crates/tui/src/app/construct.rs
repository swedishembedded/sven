// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `App` construction: turns [`AppOptions`] (parsed CLI flags, optionally a
//! loaded ATIF trajectory) into a fully-initialized [`App`], plus the
//! ATIF-record-to-chat-segment conversion it needs to replay loaded history.

use std::sync::Arc;

use sven_bootstrap::OutputBufferStore;
use tracing::debug;

use crate::{
    app::{
        agent_conn::AgentConn,
        chat_state::ChatState,
        input_state::{EditState, InputState},
        layout_cache::LayoutCache,
        nvim_state::NvimState,
        queue_state::QueueState,
        session_manager::{SessionEntry, SessionManager},
        ui_state::UiState,
        App, AppOptions,
    },
    chat::segment::ChatSegment,
    commands::{CommandRegistry, CompletionManager},
};

impl App {
    pub fn new(config: Arc<sven_config::Config>, opts: AppOptions) -> Self {
        // ── Load an ATIF trajectory (if --trace / --load-trace was given) ──────
        // `--trace PATH` is both the load source and the sync-after-every-turn
        // save target; `--load-trace PATH` alone only seeds history (matching
        // the headless runner's "load doesn't imply write-back" convention -
        // see `CiOptions::load_trace`'s doc comment). `--resume <id>` resolves
        // to `trace_path` before this point (see `src/run/tui.rs`), so it
        // needs no separate handling here.
        let trace_load_path = opts
            .trace_path
            .clone()
            .or_else(|| opts.load_trace_path.clone());
        let mut loaded_trajectory: Option<atif::Trajectory> = None;
        let initial_segments: Vec<ChatSegment> = if let Some(ref path) = trace_load_path {
            if path.exists() {
                match sven_session_store::load_session_from(path) {
                    Ok(trajectory) => {
                        let segs =
                            sven_session_store::steps_to_conversation_records(&trajectory.steps)
                                .into_iter()
                                .filter_map(conversation_record_to_chat_segment)
                                .collect();
                        loaded_trajectory = Some(trajectory);
                        segs
                    }
                    Err(e) => {
                        debug!("failed to load ATIF trajectory {}: {e}", path.display());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let initial_model_cfg = if let Some(ref mo) = opts.model_override {
            sven_model_drivers::resolve_model_from_config(&config, mo)
        } else {
            config.model.clone()
        };

        let project_root = sven_workspace::find_project_root().ok();
        let shared_skills = sven_workspace::SharedSkills::new(sven_workspace::discover_skills(
            project_root.as_deref(),
        ));
        let shared_agents = sven_workspace::SharedAgents::new(sven_workspace::discover_agents(
            project_root.as_deref(),
        ));

        let mut registry = CommandRegistry::with_builtins();
        let startup_commands = sven_workspace::discover_commands(project_root.as_deref());
        registry.register_commands(&startup_commands);
        registry.register_agents(&shared_agents.get());
        let registry = Arc::new(registry);
        let completion_manager = CompletionManager::new(registry.clone());

        let mut chat = ChatState::new();
        chat.segments = initial_segments;

        let is_node_proxy = opts.node_backend.is_some();
        let (node_url, node_token, node_insecure) = opts
            .node_backend
            .as_ref()
            .map(|nb| (Some(nb.url.clone()), Some(nb.token.clone()), nb.insecure))
            .unwrap_or((None, None, false));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(OutputBufferStore::new()));
        let shared_tools = sven_tool_registry::SharedTools::empty();
        let shared_tool_displays = sven_tool_registry::SharedToolDisplays::new();

        // ── Session manager initialization ────────────────────────────────────
        let (mut session_manager, mut initial_session_entry) = SessionManager::new();
        // If we loaded a trajectory, restore its title/status/timestamps into
        // the initial session entry so the sidebar shows the correct metadata.
        if let Some(ref trajectory) = loaded_trajectory {
            // Adopt the trajectory's own `session_id` rather than the fresh id
            // `SessionManager::new()` just generated. `SessionEntry::to_trajectory`
            // always writes `self.id` back into `trajectory.session_id` on the
            // next save; keeping a different in-memory id here would silently
            // rewrite the loaded file's `session_id` away from its filename on
            // the very first save, after which `list_sessions` (keyed on
            // `header.session_id`) and `session_path(session_id)` no longer
            // agree on which file this session lives in.
            let restored_id = trajectory
                .session_id
                .clone()
                .map(sven_session_store::SessionId::from_string)
                .unwrap_or_else(|| initial_session_entry.id.clone());
            initial_session_entry =
                SessionEntry::from_trajectory_into(trajectory, restored_id, None, false);
        }
        let active_session_id = initial_session_entry.id.clone();
        // `--trace PATH` is kept in sync after every turn; `--load-trace`-only
        // (or no flag at all) falls back to the canonical per-session path.
        let initial_session_path = opts.trace_path.clone().or_else(|| {
            sven_session_store::ensure_session_dir()
                .ok()
                .map(|dir| dir.join(format!("{}.json", active_session_id)))
        });

        // Register the initial session entry (without stored_chat - App.chat IS the chat).
        session_manager.register(initial_session_entry);

        // Load previously saved sessions from disk into the sidebar.
        session_manager.load_from_disk();

        // Ensure the new chat created at startup stays at the top of the list.
        session_manager.promote_to_top(&active_session_id);

        // Do not auto-restore the most recent session on fresh startup.
        // Start with a clean, new chat buffer. The first user message will
        // create a new chat entry as usual.
        let chat_title = loaded_trajectory
            .as_ref()
            .and_then(sven_session_store::SvenSessionMeta::from_trajectory)
            .map(|m| m.title)
            .unwrap_or_else(|| "New chat".to_string());

        let mut app = Self {
            config,
            approval: opts.approval,
            node_backend: opts.node_backend,
            is_node_proxy,
            node_url,
            node_token,
            node_insecure,
            session: crate::state::SessionState::new(initial_model_cfg, opts.mode),
            command_registry: registry,
            completion_manager,
            shared_skills,
            shared_agents,
            shared_tools,
            shared_tool_displays,
            mcp_manager: None,
            mcp_prompt_commands: std::collections::HashMap::new(),
            mcp_refresh_tx: None,
            needs_terminal_recover: false,
            buffer_store,
            chat,
            input: InputState::new(),
            edit: EditState::new(),
            queue: QueueState::new(),
            ui: UiState::new(),
            agent: AgentConn::new(),
            nvim: NvimState::new(opts.no_nvim),
            prefs: crate::app::layout_cache::SplitPrefs::new(),
            layout: LayoutCache::new(),
            sessions: session_manager,
            session_path: initial_session_path,
            chat_title,
            question_tx: None,
            question_withdrawn_tx: None,
            toast_tx: None,
        };

        for qm in opts.initial_queue {
            app.queue.messages.push_back(qm);
        }
        if let Some(prompt) = opts.initial_prompt {
            app.queue
                .messages
                .push_back(crate::QueuedMessage::plain(prompt));
        }

        // In ratatui-only mode, set default expand levels for loaded segments.
        // Tool calls, tool results, and thinking default to tier 0 (summary).
        // User and agent text default to tier 2 (full). Since the HashMap default
        // is already tier-0 for collapsible types (via default_expand_level), we
        // only need to set explicit entries for collapsible types that already
        // exist in the loaded history.
        if app.nvim.disabled {
            use crate::app::chat_state::default_expand_level;
            for (i, seg) in app.chat.segments.iter().enumerate() {
                let level = default_expand_level(seg);
                // Only insert if the default would be 0 (collapsible types).
                if level == 0 {
                    app.chat.expand_level.insert(i, 0);
                }
            }
        }

        // Bare `--resume` (no id): open the session picker immediately instead
        // of starting a normal empty chat - mirrors the `/resume` slash-command
        // handler in `submit.rs`.
        if opts.open_resume_picker {
            app.ui.session_picker_entries =
                sven_session_store::list_all_sessions(Some(200)).unwrap_or_default();
            app.ui.show_session_picker = true;
        }
        app
    }
}

// ── ConversationRecord ⇄ ChatSegment ────────────────────────────────────────

/// Convert one full-fidelity [`sven_session_store::ConversationRecord`] (as produced
/// by [`sven_session_store::steps_to_conversation_records`] from a loaded ATIF
/// trajectory) into a [`ChatSegment`], dropping system messages (the agent
/// always regenerates its own system prompt at runtime).
pub(crate) fn conversation_record_to_chat_segment(
    record: sven_session_store::ConversationRecord,
) -> Option<ChatSegment> {
    match record {
        sven_session_store::ConversationRecord::Message(m) => {
            if m.role == sven_model::Role::System {
                None
            } else {
                Some(ChatSegment::Message(m))
            }
        }
        sven_session_store::ConversationRecord::Thinking { content } => {
            Some(ChatSegment::Thinking { content })
        }
        sven_session_store::ConversationRecord::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => {
            use sven_machines::CompactionStrategyUsed;
            let strategy = match strategy.as_deref() {
                Some("emergency") => CompactionStrategyUsed::Emergency,
                Some("narrative") => CompactionStrategyUsed::Narrative,
                _ => CompactionStrategyUsed::Structured,
            };
            Some(ChatSegment::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn: turn.unwrap_or(0),
            })
        }
    }
}
