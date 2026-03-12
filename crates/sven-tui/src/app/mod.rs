// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Top-level TUI application state and event loop.

pub(crate) mod agent_conn;
pub(crate) mod agent_events;
pub(crate) mod chat_ops;
pub(crate) mod chat_state;
pub(crate) mod dispatch;
pub(crate) mod hit_test;
pub(crate) mod input_state;
pub(crate) mod layout_cache;
pub(crate) mod nvim_state;
pub(crate) mod queue_state;
pub(crate) mod session_manager;
pub(crate) mod term_events;
pub(crate) mod ui_state;

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::EventStream;
use futures::StreamExt;
use ratatui::{layout::Rect, DefaultTerminal, Frame};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_model::Message;
use sven_tools::QuestionRequest;
use tokio::sync::mpsc;
use tracing::debug;

use sven_bootstrap::OutputBufferStore;

use crate::{
    agent::{agent_task, AgentRequest},
    chat::segment::ChatSegment,
    commands::{CommandRegistry, CompletionManager},
    keys::Action,
    layout::AppLayout,
    markdown::StyledLines,
    node_agent::node_agent_task,
    nvim::NvimBridge,
    ui::{
        input_cursor_screen_pos, nvim_cursor_screen_pos, open_pane_block, ChatPane, CompletionMenu,
        ConfirmModalView, HelpOverlay, InputEditMode, InputPane, QuestionModalView, QueueItem,
        QueuePanel, SearchBar, StatusBar, ToastStack, WelcomeScreen, WhichKeyOverlay,
    },
};

pub(crate) use agent_conn::AgentConn;
pub(crate) use chat_state::ChatState;
pub(crate) use input_state::{EditState, InputState};
pub(crate) use layout_cache::{LayoutCache, SplitPrefs};
pub(crate) use nvim_state::NvimState;
pub(crate) use queue_state::QueueState;
pub(crate) use session_manager::{SessionEntry, SessionManager};
pub(crate) use ui_state::UiState;

// Re-export FocusPane at the app module level — imported from `crate::app::FocusPane`
// throughout the codebase.
pub use ui_state::FocusPane;

// ── Public types ──────────────────────────────────────────────────────────────

/// Specifies a model switch to take effect with a queued message.
#[derive(Debug, Clone)]
pub enum ModelDirective {
    SwitchTo(Box<ModelConfig>),
}

impl ModelDirective {
    pub fn into_model_config(self) -> ModelConfig {
        match self {
            ModelDirective::SwitchTo(c) => *c,
        }
    }

    /// Display label for UI (e.g. queue panel). Never panics.
    pub fn display_label(&self) -> String {
        match self {
            ModelDirective::SwitchTo(c) => format!("{}/{}", c.provider, c.name),
        }
    }
}

/// A message waiting in the queue, with optional per-message transitions.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub content: String,
    pub model_transition: Option<ModelDirective>,
    pub mode_transition: Option<AgentMode>,
}

impl QueuedMessage {
    pub fn plain(content: String) -> Self {
        Self {
            content,
            model_transition: None,
            mode_transition: None,
        }
    }
}

/// Node-proxy backend configuration for the TUI.
///
/// When set, the TUI forwards all agent interactions to a running sven node
/// over WebSocket instead of running a local agent.  The node's agent
/// has a live `P2pHandle`, so peer tools (`list_peers`, `delegate_task`, …)
/// are available.
#[derive(Debug, Clone)]
pub struct NodeBackend {
    /// WebSocket URL of the running node (e.g. `wss://127.0.0.1:18790/ws`).
    pub url: String,
    /// Bearer token for the node's HTTP API.
    pub token: String,
    /// Skip TLS certificate verification (safe on loopback).
    pub insecure: bool,
}

/// Options passed when constructing the TUI app.
pub struct AppOptions {
    pub mode: AgentMode,
    pub initial_prompt: Option<String>,
    pub initial_history: Option<(Vec<ChatSegment>, PathBuf)>,
    pub no_nvim: bool,
    pub model_override: Option<String>,
    pub jsonl_path: Option<PathBuf>,
    pub jsonl_load_path: Option<PathBuf>,
    pub initial_queue: Vec<QueuedMessage>,
    /// When `Some`, connect the TUI to a running node instead of running a
    /// local agent.  Gives the TUI full access to the node's P2P tools.
    pub node_backend: Option<NodeBackend>,
    /// Load an existing YAML chat document to resume the conversation.
    pub chat_path: Option<PathBuf>,
    /// Save the chat to this YAML path (for headless/CI mode output).
    pub output_chat_path: Option<PathBuf>,
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
    pub(crate) shared_skills: sven_runtime::SharedSkills,
    pub(crate) shared_agents: sven_runtime::SharedAgents,
    /// Shared tool snapshot — populated by AgentBuilder after the local tool
    /// registry is built.  Empty in node-proxy mode (tools are fetched live
    /// from the node when `/tools` is opened).
    pub(crate) shared_tools: sven_tools::SharedTools,
    /// Tool display registry — set by AgentBuilder after the registry is built.
    /// Used for chat view (collapsed summary, display name) when present.
    pub(crate) shared_tool_displays: sven_tools::SharedToolDisplays,
    pub(crate) history_path: Option<PathBuf>,
    pub(crate) jsonl_path: Option<PathBuf>,
    /// Set to `true` after a tool call completes — triggers a terminal-state
    /// recovery pass before the next draw.
    pub(crate) needs_terminal_recover: bool,
    /// Shared output buffer store — also held by the agent's `TaskTool` so that
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
    /// Multi-session manager — holds all chat sessions and the shared event mux.
    pub(crate) sessions: SessionManager,
    /// Path to the YAML chat document for the current active session.
    pub(crate) yaml_path: Option<PathBuf>,
    /// Title of the current active chat session.
    pub(crate) chat_title: String,
    /// Shared question sender — cloned into every agent task so that question
    /// requests from all sessions are routed through the single `question_rx`
    /// in `run()`.  `None` before `run()` is called (e.g. in tests).
    pub(crate) question_tx: Option<mpsc::Sender<QuestionRequest>>,
}

impl App {
    pub fn new(config: Arc<Config>, opts: AppOptions) -> Self {
        let (initial_segments, history_path) = opts
            .initial_history
            .map(|(segs, path)| (segs, Some(path)))
            .unwrap_or_else(|| (Vec::new(), None));

        let initial_segments = if let Some(ref jsonl) = opts.jsonl_path {
            if jsonl.exists() {
                match std::fs::read_to_string(jsonl) {
                    Ok(content) => match sven_input::parse_jsonl_full(&content) {
                        Ok(parsed) => parsed
                            .records
                            .into_iter()
                            .filter_map(|r| match r {
                                sven_input::ConversationRecord::Message(m) => {
                                    if m.role != sven_model::Role::System {
                                        Some(ChatSegment::Message(m))
                                    } else {
                                        None
                                    }
                                }
                                sven_input::ConversationRecord::Thinking { content } => {
                                    Some(ChatSegment::Thinking { content })
                                }
                                sven_input::ConversationRecord::ContextCompacted {
                                    tokens_before,
                                    tokens_after,
                                    strategy,
                                    turn,
                                } => {
                                    use sven_core::CompactionStrategyUsed;
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
                            })
                            .collect(),
                        Err(e) => {
                            debug!("failed to parse JSONL conversation file: {e}");
                            initial_segments
                        }
                    },
                    Err(e) => {
                        debug!("failed to read JSONL conversation file: {e}");
                        initial_segments
                    }
                }
            } else {
                initial_segments
            }
        } else {
            initial_segments
        };

        let initial_model_cfg = if let Some(ref mo) = opts.model_override {
            sven_model::resolve_model_from_config(&config, mo)
        } else {
            config.model.clone()
        };

        let project_root = sven_runtime::find_project_root().ok();
        let shared_skills =
            sven_runtime::SharedSkills::new(sven_runtime::discover_skills(project_root.as_deref()));
        let shared_agents =
            sven_runtime::SharedAgents::new(sven_runtime::discover_agents(project_root.as_deref()));

        let mut registry = CommandRegistry::with_builtins();
        let startup_commands = sven_runtime::discover_commands(project_root.as_deref());
        registry.register_commands(&startup_commands);
        registry.register_agents(&shared_agents.get());
        let registry = Arc::new(registry);
        let completion_manager = CompletionManager::new(registry.clone());

        let jsonl_path = opts
            .jsonl_path
            .or_else(|| opts.jsonl_load_path.clone())
            .or_else(sven_runtime::resolve_auto_log_path);

        // ── Load YAML chat document (if --chat / --load-chat was specified) ──
        // Only load from YAML when the segments are still empty (JSONL loading
        // takes priority when both --jsonl and --chat are specified).
        // `loaded_doc` captures the parsed document so we can restore its
        // metadata (title, status, timestamps) into the initial SessionEntry.
        let mut loaded_doc: Option<sven_input::ChatDocument> = None;
        let initial_segments = if initial_segments.is_empty() {
            if let Some(ref yaml_path) = opts.chat_path {
                if yaml_path.exists() {
                    match std::fs::read_to_string(yaml_path) {
                        Ok(content) => match sven_input::parse_chat_document(&content) {
                            Ok(doc) => {
                                let segs = sven_input::turns_to_messages(&doc.turns)
                                    .into_iter()
                                    .filter(|m| m.role != sven_model::Role::System)
                                    .map(ChatSegment::Message)
                                    .collect();
                                loaded_doc = Some(doc);
                                segs
                            }
                            Err(e) => {
                                debug!("failed to parse YAML chat document: {e}");
                                Vec::new()
                            }
                        },
                        Err(e) => {
                            debug!("failed to read YAML chat document: {e}");
                            Vec::new()
                        }
                    }
                } else {
                    initial_segments
                }
            } else {
                initial_segments
            }
        } else {
            initial_segments
        };

        let mut chat = ChatState::new();
        chat.segments = initial_segments;

        let is_node_proxy = opts.node_backend.is_some();
        let (node_url, node_token, node_insecure) = opts
            .node_backend
            .as_ref()
            .map(|nb| (Some(nb.url.clone()), Some(nb.token.clone()), nb.insecure))
            .unwrap_or((None, None, false));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(OutputBufferStore::new()));
        let shared_tools = sven_tools::SharedTools::empty();
        let shared_tool_displays = sven_tools::SharedToolDisplays::new();

        // ── Session manager initialization ────────────────────────────────────
        let (mut session_manager, mut initial_session_entry) = SessionManager::new();
        // If we loaded a YAML document, restore its title/status/timestamps into
        // the initial session entry so the sidebar shows the correct metadata.
        if let Some(ref doc) = loaded_doc {
            initial_session_entry =
                SessionEntry::from_document_into(doc, initial_session_entry.id.clone());
        }
        let active_session_id = initial_session_entry.id.clone();
        let initial_yaml_path = opts
            .output_chat_path
            .clone()
            .or_else(|| opts.chat_path.clone())
            .or_else(|| {
                sven_input::ensure_chat_dir()
                    .ok()
                    .map(|dir| dir.join(format!("{}.yaml", active_session_id)))
            });

        // Register the initial session entry (without stored_chat — App.chat IS the chat).
        session_manager.register(initial_session_entry);

        // Load previously saved sessions from disk into the sidebar.
        session_manager.load_from_disk();

        // Ensure the new chat created at startup stays at the top of the list.
        session_manager.promote_to_top(&active_session_id);

        // Do not auto-restore the most recent session on fresh startup.
        // Start with a clean, new chat buffer. The first user message will
        // create a new chat entry as usual.
        let chat_title = loaded_doc
            .as_ref()
            .map(|d| d.title.clone())
            .unwrap_or_else(|| "New chat".to_string());

        let mut app = Self {
            config,
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
            history_path,
            jsonl_path,
            needs_terminal_recover: false,
            buffer_store,
            chat,
            input: InputState::new(),
            edit: EditState::new(),
            queue: QueueState::new(),
            ui: UiState::new(),
            agent: AgentConn::new(),
            nvim: NvimState::new(opts.no_nvim),
            prefs: SplitPrefs::new(),
            layout: LayoutCache::new(),
            sessions: session_manager,
            yaml_path: initial_yaml_path,
            chat_title,
            question_tx: None,
        };

        for qm in opts.initial_queue {
            app.queue.messages.push_back(qm);
        }
        if let Some(prompt) = opts.initial_prompt {
            app.queue.messages.push_back(QueuedMessage::plain(prompt));
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
        app
    }

    // ── Render ────────────────────────────────────────────────────────────────

    /// Render the entire TUI into `frame`.
    ///
    /// `nvim_lines`      — rendered Neovim grid lines (empty when no nvim).
    /// `nvim_draw_scroll`— scroll offset used when drawing nvim lines.
    /// `nvim_cursor`     — Neovim cursor position (row, col) in grid space.
    pub(crate) fn view(
        &mut self,
        frame: &mut Frame,
        nvim_lines: &StyledLines,
        nvim_draw_scroll: u16,
        nvim_cursor: Option<(u16, u16)>,
    ) {
        let ascii = self.ascii();
        // ── Full-screen inspector overlay (early return) ──────────────────────
        if let Some(inspector) = &mut self.ui.inspector {
            inspector.pager.render(
                frame,
                &self.ui.search.matches,
                self.ui.search.current,
                &self.ui.search.query,
                self.ui.search.regex.as_ref(),
                ascii,
            );
            if self.ui.search.active {
                let area = frame.area();
                let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                frame.render_widget(
                    SearchBar {
                        query: &self.ui.search.query,
                        match_count: self.ui.search.matches.len(),
                        current_match: self.ui.search.current,
                    },
                    search_area,
                );
            }
            return;
        }
        // ── Full-screen pager (early return) ──────────────────────────────────
        if let Some(pager) = &mut self.ui.pager {
            pager.render(
                frame,
                &self.ui.search.matches,
                self.ui.search.current,
                &self.ui.search.query,
                self.ui.search.regex.as_ref(),
                ascii,
            );
            if self.ui.search.active {
                let area = frame.area();
                let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                frame.render_widget(
                    SearchBar {
                        query: &self.ui.search.query,
                        match_count: self.ui.search.matches.len(),
                        current_match: self.ui.search.current,
                    },
                    search_area,
                );
            }
            return;
        }

        // Compute a dynamic input height that expands with content up to 50% of
        // the screen, but never shrinks below the user-preferred minimum.
        let area = frame.area();
        let max_input_height = (area.height / 2).max(3);
        let prompt_width: u16 = 2; // `> ` prefix
        let avail_wrap_width = area.width.saturating_sub(prompt_width).max(1) as usize;
        let in_edit_for_height =
            self.edit.queue_index.is_some() || self.edit.message_index.is_some();
        let content_for_height = if in_edit_for_height {
            &self.edit.buffer
        } else {
            &self.input.buffer
        };
        let wrap = crate::input_wrap::wrap_content(
            content_for_height,
            avail_wrap_width,
            content_for_height.len(),
        );
        let text_lines = wrap.lines.len().max(1) as u16;
        let attach_rows = self.input.attachments.len() as u16;
        let desired_input_height = (text_lines + attach_rows + 2) // +2 for top/bottom borders
            .max(self.prefs.input_height)
            .min(max_input_height);
        let layout = AppLayout::new(
            frame,
            self.ui.search.active,
            self.queue.messages.len(),
            desired_input_height,
            self.prefs.effective_chat_list_width(),
            self.prefs.effective_peers_pane_height(),
        );
        // Clean up expired toasts every frame.
        self.ui.prune_toasts();

        // ── Status bar ────────────────────────────────────────────────────────
        // In node-proxy mode the node owns model/mode; show "node" as model label.
        let status_model_name: &str = if self.is_node_proxy {
            "node"
        } else {
            &self.session.model_display
        };
        let in_edit = self.edit.active();

        // Compute team progress for the status bar.
        let task_progress: Option<(usize, usize)> = None; // TODO: wire up from team store

        // Viewing-teammate name for status bar hint.
        let viewing_teammate: Option<&str> =
            self.ui.active_session_peer.as_ref().and_then(|peer_id| {
                self.ui
                    .team_picker_entries
                    .iter()
                    .find(|e| e.peer_id == *peer_id)
                    .map(|e| e.name.as_str())
            });

        let team_active_count = self
            .ui
            .team_picker_entries
            .iter()
            .filter(|e| !e.is_local && matches!(e.status, crate::ui::AgentPickerStatus::Active))
            .count() as u8;

        frame.render_widget(
            StatusBar {
                model_name: status_model_name,
                mode: self.session.mode,
                context_pct: self.agent.context_pct,
                total_context_pct: self.agent.total_context_pct,
                total_context_tokens: self.agent.total_context_tokens,
                total_output_tokens: self.agent.total_output_tokens,
                cache_hit_pct: self.agent.cache_hit_pct,
                agent_busy: self.agent.busy,
                current_tool: self.agent.current_tool.as_deref(),
                ascii,
                focus: self.ui.focus,
                spinner_frame: self.agent.spinner_frame,
                streaming_tokens: self.agent.streaming_tokens,
                in_edit,
                in_search: self.ui.search.active,
                team_name: self.ui.team_name.as_deref(),
                team_role: None, // TODO: wire from team config
                team_active_count,
                task_progress,
                viewing_teammate,
            },
            layout.status_bar,
        );

        // ── Chat pane ─────────────────────────────────────────────────────────
        // Show the welcome screen when the chat is empty and the agent is idle.
        let show_welcome = self.chat.segments.is_empty()
            && self.chat.streaming_buffer.is_empty()
            && !self.agent.busy
            && self.nvim.disabled;

        if show_welcome {
            let mode_label = self.session.mode.to_string();
            let mode_style = crate::ui::theme::mode_style(self.session.mode);
            frame.render_widget(
                WelcomeScreen {
                    model_name: &self.session.model_display,
                    mode_label: &mode_label,
                    mode_style,
                },
                layout.chat_pane,
            );
        }

        let lines_to_draw = if !nvim_lines.is_empty() {
            nvim_lines
        } else {
            &self.chat.lines
        };
        let editing_range = self
            .edit
            .message_index
            .and_then(|idx| self.chat.segment_line_ranges.get(idx))
            .copied();

        let auto_scroll_paused = !self.chat.auto_scroll && !self.chat.lines.is_empty();
        let highlight_line_range = (self.ui.focus == FocusPane::Chat && self.nvim.disabled)
            .then_some(self.chat.focused_segment)
            .flatten()
            .and_then(|idx| self.chat.segment_line_ranges.get(idx).copied());
        if !show_welcome {
            frame.render_widget(
                ChatPane {
                    lines: lines_to_draw,
                    scroll_offset: nvim_draw_scroll,
                    focused: self.ui.focus == FocusPane::Chat,
                    ascii,
                    search_query: &self.ui.search.query,
                    search_matches: &self.ui.search.matches,
                    search_current: self.ui.search.current,
                    search_regex: self.ui.search.regex.as_ref(),
                    editing_line_range: editing_range,
                    no_nvim: self.nvim.disabled,
                    segment_count: self.chat.segments.len(),
                    auto_scroll_paused,
                    selection: self.chat.normalized_selection(),
                    highlight_line_range,
                },
                layout.chat_pane,
            );
        } // end if !show_welcome

        // Neovim cursor (placed after chat widget renders).
        if let Some(cursor) = nvim_cursor {
            let block_inner = {
                let block = open_pane_block("Chat", self.ui.focus == FocusPane::Chat, ascii);
                block.inner(layout.chat_pane)
            };
            if let Some(pos) = nvim_cursor_screen_pos(
                block_inner,
                cursor,
                nvim_draw_scroll,
                self.ui.focus == FocusPane::Chat,
            ) {
                frame.set_cursor_position(pos);
            }
        }

        // ── Input pane ────────────────────────────────────────────────────────
        let edit_mode = if self.edit.queue_index.is_some() {
            InputEditMode::Queue
        } else if self.edit.message_index.is_some() {
            InputEditMode::Segment
        } else {
            InputEditMode::Normal
        };
        let in_edit = edit_mode != InputEditMode::Normal;
        let (content, cursor_pos, scroll) = if in_edit {
            (
                self.edit.buffer.as_str(),
                self.edit.cursor,
                self.edit.scroll_offset,
            )
        } else {
            (
                self.input.buffer.as_str(),
                self.input.cursor,
                self.input.scroll_offset,
            )
        };

        // Suppress input cursor when a modal owns it.
        let input_cursor_active = (self.ui.focus == FocusPane::Input || in_edit)
            && self.ui.question_modal.is_none()
            && self.ui.confirm_modal.is_none();

        frame.render_widget(
            InputPane {
                content,
                cursor_pos,
                scroll_offset: scroll,
                focused: self.ui.focus == FocusPane::Input || in_edit,
                ascii,
                edit_mode,
                attachments: &self.input.attachments,
                is_resizing: matches!(
                    self.layout.resize_drag,
                    Some(crate::app::layout_cache::ResizeDrag::InputHeight { .. })
                ),
            },
            layout.input_pane,
        );

        if input_cursor_active {
            if let Some(pos) = input_cursor_screen_pos(
                layout.input_pane,
                content,
                cursor_pos,
                scroll,
                true,
                ascii,
                edit_mode,
                self.input.attachments.len(),
            ) {
                frame.set_cursor_position(pos);
            }
        }

        // ── Queue panel ───────────────────────────────────────────────────────
        if !self.queue.messages.is_empty() {
            let items: Vec<QueueItem> = self
                .queue
                .messages
                .iter()
                .map(|qm| QueueItem {
                    content: &qm.content,
                    model_label: qm.model_transition.as_ref().map(|d| {
                        // Leak to 'static for the widget lifetime — the queue
                        // lives in self which outlives this frame.
                        Box::leak(d.display_label().into_boxed_str()) as &str
                    }),
                    mode_label: qm.mode_transition,
                })
                .collect();
            frame.render_widget(
                QueuePanel {
                    items: &items,
                    selected: self.queue.selected,
                    editing: self.edit.queue_index,
                    focused: self.ui.focus == FocusPane::Queue,
                    ascii,
                },
                layout.queue_pane,
            );
        }

        // ── Chat list pane (right side) ───────────────────────────────────────
        if self.prefs.chat_list_visible && layout.chat_list_pane.width > 0 {
            let tree_rows = self.sessions.tree_rows();
            let items = crate::ui::build_chat_list_items(
                &tree_rows,
                &self.sessions.entries,
                &self.sessions.active_id,
                self.agent.anim_frame,
                self.agent.busy,
            );
            let cl_focused = self.ui.focus == FocusPane::ChatList;
            let chat_list_scroll_offset = self.chat_list_scroll_offset();
            frame.render_widget(
                crate::ui::ChatListPane {
                    items: &items,
                    selected: self.sessions.list_selected,
                    focused: cl_focused,
                    ascii,
                    scroll_offset: chat_list_scroll_offset,
                    is_resizing: matches!(
                        self.layout.resize_drag,
                        Some(crate::app::layout_cache::ResizeDrag::ChatListWidth { .. })
                    ),
                },
                layout.chat_list_pane,
            );
        }
        // ── Peers pane (right side, below chat list) ───────────────────────────
        if layout.peers_pane.height > 0 && layout.peers_pane.width > 0 {
            let items: Vec<crate::ui::PeerListItem<'_>> = self
                .ui
                .peers
                .iter()
                .map(|peer| crate::ui::PeerListItem {
                    name: &peer.name,
                    connected: peer.connected,
                    can_delegate: peer.can_delegate,
                })
                .collect();
            let peers_focused = self.ui.focus == FocusPane::Peers;
            let peers_scroll_offset = self.peers_scroll_offset();
            frame.render_widget(
                crate::ui::PeersPane {
                    items: &items,
                    selected: self.ui.peers_selected,
                    focused: peers_focused,
                    ascii,
                    scroll_offset: peers_scroll_offset,
                    is_resizing: matches!(
                        self.layout.resize_drag,
                        Some(crate::app::layout_cache::ResizeDrag::PeersSplit { .. })
                    ),
                },
                layout.peers_pane,
            );
        }

        // ── Completion overlay ────────────────────────────────────────────────
        if let Some(ref overlay) = self.ui.completion {
            frame.render_widget(
                CompletionMenu {
                    overlay,
                    input_pane: layout.input_pane,
                    ascii,
                },
                frame.area(),
            );
        }

        // ── Search bar ────────────────────────────────────────────────────────
        if self.ui.search.active {
            frame.render_widget(
                SearchBar {
                    query: &self.ui.search.query,
                    match_count: self.ui.search.matches.len(),
                    current_match: self.ui.search.current,
                },
                layout.search_bar,
            );
        }

        // ── Help overlay ──────────────────────────────────────────────────────
        if self.ui.show_help {
            frame.render_widget(HelpOverlay { ascii }, frame.area());
        }

        // ── Team picker overlay ───────────────────────────────────────────────
        if self.ui.show_team_picker {
            let team_name = self.ui.team_name.as_deref().unwrap_or("(no team)");
            frame.render_widget(
                crate::ui::TeamPickerOverlay {
                    entries: &self.ui.team_picker_entries,
                    state: &mut self.ui.team_picker_state,
                    team_name,
                    ascii,
                },
                frame.area(),
            );
        }

        // ── Question modal ────────────────────────────────────────────────────
        if let Some(modal) = &self.ui.question_modal {
            let result = QuestionModalView {
                questions: &modal.questions,
                current_q: modal.current_q,
                selected_options: &modal.selected_options,
                other_selected: modal.other_selected,
                other_input: &modal.other_input,
                other_cursor: modal.other_cursor,
                focused_option: modal.focused_option,
                ascii,
            }
            .render_with_cursor(frame.area(), frame.buffer_mut());
            if let Some(pos) = result.pos {
                frame.set_cursor_position(pos);
            }
        }

        // ── Confirm modal ─────────────────────────────────────────────────────
        if let Some(modal) = &self.ui.confirm_modal {
            frame.render_widget(
                ConfirmModalView {
                    title: &modal.title,
                    message: &modal.message,
                    confirm_label: &modal.confirm_label,
                    cancel_label: &modal.cancel_label,
                    focused_button: modal.focused_button,
                    has_action: modal.has_action(),
                    ascii,
                    border_color: modal.border_color,
                },
                frame.area(),
            );
        }

        // ── Which-key popup (Ctrl+w chord hint) ──────────────────────────────
        if self.ui.pending_nav {
            frame.render_widget(WhichKeyOverlay { ascii }, frame.area());
        }

        // ── Toast notifications ───────────────────────────────────────────────
        if !self.ui.toasts.is_empty() {
            frame.render_widget(
                ToastStack {
                    toasts: &self.ui.toasts,
                    ascii,
                },
                frame.area(),
            );
        }
    }

    // ── Run loop ──────────────────────────────────────────────────────────────

    /// Run the TUI event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(4);

        // Store the sender so that agents spawned for new/switched-to sessions
        // all route their questions through the same handler in the run loop.
        self.question_tx = Some(question_tx.clone());

        self.agent.tx = Some(submit_tx.clone());
        // Register the initial session's agent channels in its entry so that
        // switch_session() finds them and does NOT spawn a second agent when the
        // user switches away and then returns to this session.
        let initial_id = self.sessions.active_id.clone();
        if let Some(entry) = self.sessions.get_mut(&initial_id) {
            entry.agent_tx = Some(submit_tx.clone());
            entry.agent_cancel = self.agent.cancel.clone();
        }
        // Remove per-agent event_rx — events now flow through the mux channel.
        // Set up a forwarding task: per-session events → (SessionId, AgentEvent) mux.
        let active_id = self.sessions.active_id.clone();
        let mux_tx = self.sessions.multi_event_tx.clone();
        tokio::spawn(async move {
            let mut rx = event_rx;
            while let Some(event) = rx.recv().await {
                if mux_tx.send((active_id.clone(), event)).await.is_err() {
                    break;
                }
            }
        });

        if let Some(nb) = self.node_backend.take() {
            // Node-proxy mode: forward all agent interactions to the running
            // node over WebSocket.  The node's agent has a live
            // P2pHandle, so peer tools are available.
            let cancel_handle_task = self.agent.cancel.clone();
            tokio::spawn(async move {
                node_agent_task(
                    nb.url,
                    nb.token,
                    nb.insecure,
                    submit_rx,
                    event_tx,
                    cancel_handle_task,
                )
                .await;
            });
        } else {
            let cfg = self.config.clone();
            let mode = self.session.mode;
            let startup_model_cfg = self.session.model_cfg.clone();
            let cancel_handle_task = self.agent.cancel.clone();
            let shared_skills_task = self.shared_skills.clone();
            let shared_agents_task = self.shared_agents.clone();
            let shared_tools_task = self.shared_tools.clone();
            let shared_tool_displays_task = self.shared_tool_displays.clone();
            let buffer_store_task = Arc::clone(&self.buffer_store);
            tokio::spawn(async move {
                agent_task(
                    cfg,
                    startup_model_cfg,
                    mode,
                    submit_rx,
                    event_tx,
                    question_tx,
                    cancel_handle_task,
                    shared_skills_task,
                    shared_agents_task,
                    shared_tools_task,
                    shared_tool_displays_task,
                    buffer_store_task,
                )
                .await;
            });
        }

        // In node-proxy mode, request the initial peer list.
        if self.is_node_proxy {
            let submit_tx = self.agent.tx.clone();
            if let Some(ref tx) = submit_tx {
                let _ = tx.try_send(AgentRequest::ListPeers);
            }
        }

        if !self.chat.segments.is_empty() {
            let messages: Vec<Message> = self
                .chat
                .segments
                .iter()
                .filter_map(|seg| {
                    if let ChatSegment::Message(m) = seg {
                        Some(m.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if !messages.is_empty() {
                let _ = submit_tx.send(AgentRequest::LoadHistory(messages)).await;
            }
            self.rerender_chat().await;
            if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    false,
                    self.queue.messages.len(),
                    self.prefs.input_height,
                    self.prefs.effective_chat_list_width(),
                    self.prefs.effective_peers_pane_height(),
                );
                self.layout.chat_height = layout.chat_inner_height().max(1);
            }
            self.scroll_to_bottom();
        }

        if !self.nvim.disabled {
            let (nvim_width, nvim_height) = if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    false,
                    0,
                    self.prefs.input_height,
                    self.prefs.effective_chat_list_width(),
                    self.prefs.effective_peers_pane_height(),
                );
                (
                    layout.chat_pane.width.saturating_sub(2),
                    layout.chat_inner_height().max(1),
                )
            } else {
                (80, 24)
            };

            match NvimBridge::spawn(nvim_width, nvim_height).await {
                Ok(mut bridge) => {
                    if let Err(e) = bridge.configure_buffer().await {
                        tracing::warn!("Failed to configure Neovim buffer: {}", e);
                    }
                    self.nvim.flush_notify = Some(bridge.flush_notify.clone());
                    self.nvim.submit_notify = Some(bridge.submit_notify.clone());
                    self.nvim.quit_notify = Some(bridge.quit_notify.clone());
                    self.nvim.bridge = Some(Arc::new(tokio::sync::Mutex::new(bridge)));
                }
                Err(e) => {
                    tracing::error!("Failed to spawn Neovim: {}. Chat view will be degraded.", e);
                }
            }

            if self.nvim.bridge.is_some() && !self.chat.segments.is_empty() {
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
        }

        if let Some(qm) = self.queue.messages.pop_front() {
            self.chat
                .segments
                .push(ChatSegment::Message(Message::user(&qm.content)));
            self.rerender_chat().await;
            self.send_to_agent(qm).await;
        }

        let mut crossterm_events = EventStream::new();

        // Animation tick: fires every 80 ms while the agent is busy, giving
        // smooth 12-fps animations without spinning when the agent is idle.
        let mut anim_tick = tokio::time::interval(tokio::time::Duration::from_millis(80));
        anim_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Enable bracketed paste and push keyboard enhancement flags to stdout.
        //
        // We also push the flags in main.rs (via stderr, before stderr is
        // redirected to /dev/null), but some terminals tie keyboard state to
        // the specific fd / alternate-screen session.  Sending them again to
        // stdout here guarantees they are active regardless.
        //
        // REPORT_ALL_KEYS_AS_ESCAPE_CODES makes even plain Enter arrive as
        // `\x1b[13u`, ensuring every Enter variant carries its modifiers
        // (plain Enter = no modifiers, Shift+Enter = modifier 2, etc.) and
        // is parsed by crossterm as a distinct event.
        {
            use crossterm::event::{
                EnableBracketedPaste, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
            };
            let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
            let _ = crossterm::execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                )
            );
        }

        loop {
            // ── Layout cache update ───────────────────────────────────────────
            if let Ok(size) = terminal.size() {
                let prompt_width: u16 = 2;
                let avail_wrap_width = size.width.saturating_sub(prompt_width).max(1) as usize;
                let in_edit = self.edit.queue_index.is_some() || self.edit.message_index.is_some();
                let content_str = if in_edit {
                    &self.edit.buffer
                } else {
                    &self.input.buffer
                };
                let wrap_est = crate::input_wrap::wrap_content(
                    content_str,
                    avail_wrap_width,
                    content_str.len(),
                );
                let text_lines = wrap_est.lines.len().max(1) as u16;
                let attach_rows = self.input.attachments.len() as u16;
                let max_input_height = (size.height / 2).max(3);
                let desired_input_height = (text_lines + attach_rows + 2)
                    .max(self.prefs.input_height)
                    .min(max_input_height);
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.ui.search.active,
                    self.queue.messages.len(),
                    desired_input_height,
                    self.prefs.effective_chat_list_width(),
                    self.prefs.effective_peers_pane_height(),
                );
                self.layout.chat_height = layout.chat_inner_height().max(1);
                let max_scroll =
                    (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
                if self.chat.scroll_offset > max_scroll {
                    self.chat.scroll_offset = max_scroll;
                }
                self.layout.chat_pane = layout.chat_pane;
                // Open-border chat: no left/right `│`, full width available.
                self.layout.chat_inner_width = layout.chat_pane.width.max(20);
                // Input: no left/right borders, but 2 cols reserved for `>` prompt.
                self.layout.input_inner_width = layout.input_pane.width.saturating_sub(2);
                self.layout.input_inner_height = layout.input_pane.height.saturating_sub(2);
                self.layout.input_pane = layout.input_pane;
                self.layout.queue_pane = layout.queue_pane;
                self.layout.chat_list_pane = layout.chat_list_pane;
                self.layout.peers_pane = layout.peers_pane;
            }

            // ── Cursor scroll adjustment ──────────────────────────────────────
            if self.edit.message_index.is_some() {
                self.adjust_edit_scroll();
            } else {
                self.adjust_input_scroll();
            }

            // ── Terminal recovery after tool calls ────────────────────────────
            if self.needs_terminal_recover {
                self.needs_terminal_recover = false;
                use crossterm::{
                    event::{
                        EnableMouseCapture, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
                    },
                    execute,
                };
                let raw_was_disabled = !crossterm::terminal::is_raw_mode_enabled().unwrap_or(true);
                if raw_was_disabled {
                    let _ = crossterm::terminal::enable_raw_mode();
                    let _ = terminal.clear();
                }
                let _ = execute!(std::io::stdout(), EnableMouseCapture);
                let _ = execute!(
                    std::io::stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                            | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    )
                );
            }

            // ── Compute Neovim render data (async, before draw) ───────────────
            let (nvim_lines, nvim_draw_scroll, nvim_cursor) =
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let bridge = nvim_bridge.lock().await;
                    let lines = bridge.render_to_lines(0, bridge.height).await;
                    let cursor = bridge.get_cursor_pos().await;
                    (lines, 0u16, Some(cursor))
                } else {
                    (Vec::new(), self.chat.scroll_offset, None)
                };

            // ── Draw ──────────────────────────────────────────────────────────
            terminal.draw(|frame| {
                self.view(frame, &nvim_lines, nvim_draw_scroll, nvim_cursor);
            })?;

            // ── Event select ──────────────────────────────────────────────────
            let flush_notify_clone = self.nvim.flush_notify.clone();
            let submit_notify_clone = self.nvim.submit_notify.clone();
            let quit_notify_clone = self.nvim.quit_notify.clone();
            tokio::select! {
                Some((session_id, agent_event)) = self.recv_agent_event() => {
                    if self.handle_agent_event(session_id, agent_event).await { break; }
                }
                Some(Ok(term_event)) = crossterm_events.next() => {
                    if self.handle_term_event(term_event).await { break; }
                }
                Some(req) = question_rx.recv() => {
                    self.handle_question_request(req);
                }
                _ = anim_tick.tick(), if self.agent.busy || self.sessions.any_background_busy() => {
                    // Advance the clock-driven animation frame and rebuild the
                    // display so animated indicators update at a steady 80ms rate.
                    self.agent.anim_frame = self.agent.anim_frame.wrapping_add(1);
                    self.build_display_from_segments();
                    self.ui.search.update_matches(&self.chat.lines);
                }
                _ = Self::nvim_notify_future(flush_notify_clone.as_deref()) => {}
                _ = Self::nvim_notify_future(submit_notify_clone.as_deref()) => {
                    let _ = self.dispatch(Action::SubmitBufferToAgent).await;
                }
                _ = Self::nvim_notify_future(quit_notify_clone.as_deref()) => {
                    break;
                }
            }
        }

        // Synchronous final save so messages are never lost on clean exit.
        // tokio::spawn tasks queued by save_history_async may not execute if the
        // runtime drops immediately after run() returns, so we flush here.
        self.save_history_sync();

        Ok(())
    }

    pub(crate) async fn recv_agent_event(&mut self) -> Option<(sven_input::SessionId, AgentEvent)> {
        self.sessions.multi_event_rx.recv().await
    }

    // ── Multi-session operations ──────────────────────────────────────────────

    /// Create a new chat session, make it active, and clear the current chat state.
    pub(crate) async fn new_session(&mut self) {
        // Snapshot active chat into its session entry before creating the new one.
        self.save_active_to_session_entry();

        // Create the session entry and make it active.
        let new_id = self.sessions.create_session("New chat");
        self.sessions.active_id = new_id.clone();

        // Reset live chat/agent/input/queue/edit state for the new empty session.
        self.chat = ChatState::new();
        self.agent = AgentConn::new();
        self.session = crate::state::SessionState::new(self.config.model.clone(), AgentMode::Agent);
        self.input = InputState::new();
        // Clear any in-flight queue and edit state from the previous session so
        // queued messages don't bleed into the new session's agent.
        self.queue = QueueState::new();
        self.edit = EditState::new();
        // New TUI sessions use YAML persistence only; no JSONL path.
        self.jsonl_path = None;

        // Spawn an agent task; sets self.agent.tx and the entry's agent_tx/cancel.
        self.spawn_agent_for_session(&new_id).await;

        self.chat_title = "New chat".to_string();
        self.yaml_path = sven_input::ensure_chat_dir()
            .ok()
            .map(|dir| dir.join(format!("{}.yaml", new_id)));

        self.sessions.promote_to_top(&new_id);
        self.sessions.sync_list_selection_to_active();
        self.rerender_chat().await;
        self.ui.focus = FocusPane::Input;
    }

    /// Switch to a different session.
    pub(crate) async fn switch_session(&mut self, target_id: sven_input::SessionId) {
        // Save active state.
        self.save_active_to_session_entry();

        // Swap in the target session's stored state.
        let target_chat = self
            .sessions
            .get_mut(&target_id)
            .and_then(|e| e.stored_chat.take());

        // If the target session has no stored chat, try to load from disk.
        // Also refresh session entry metadata (title, status, created_at) from
        // the full document — load_from_disk only has ChatEntry approximations.
        let target_chat = target_chat.or_else(|| {
            let yaml_path = self.sessions.get(&target_id)?.yaml_path.clone()?;
            if yaml_path.exists() {
                let content = std::fs::read_to_string(&yaml_path).ok()?;
                let doc = sven_input::parse_chat_document(&content).ok()?;
                let segments: Vec<crate::chat::segment::ChatSegment> =
                    sven_input::turns_to_messages(&doc.turns)
                        .into_iter()
                        .filter(|m| m.role != sven_model::Role::System)
                        .map(crate::chat::segment::ChatSegment::Message)
                        .collect();
                // Refresh entry metadata from the full document.
                if let Some(entry) = self.sessions.get_mut(&target_id) {
                    let refreshed = SessionEntry::from_document(&doc);
                    entry.title = refreshed.title;
                    entry.status = refreshed.status;
                    entry.created_at = refreshed.created_at;
                    entry.updated_at = refreshed.updated_at;
                }
                let mut chat = ChatState::new();
                chat.segments = segments;
                Some(chat)
            } else {
                None
            }
        });

        // Subagent sessions get their stored_chat populated via SubagentEvent updates
        // (ACP-based subagents).  As a fallback for legacy subagent sessions that
        // pre-date the ACP rewrite, read the raw buffer content.  This path should
        // rarely be needed in practice.
        let target_chat = if target_chat.is_none() {
            let buffer_handle = self
                .sessions
                .get(&target_id)
                .and_then(|e| e.buffer_handle.clone());
            if let Some(handle) = buffer_handle {
                let store = self.buffer_store.lock().await;
                let content = store.read_all(&handle).unwrap_or_default();
                drop(store);
                if !content.is_empty() {
                    use sven_model::{Message, MessageContent, Role};
                    let seg = ChatSegment::Message(Message {
                        role: Role::Assistant,
                        content: MessageContent::Text(content),
                    });
                    let mut chat = ChatState::new();
                    chat.segments = vec![seg];
                    Some(chat)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            target_chat
        };

        let old_chat = std::mem::replace(&mut self.chat, target_chat.unwrap_or_default());
        let _ = old_chat; // already saved to session entry

        // Restore per-session model/mode state.
        let target_session_state = self
            .sessions
            .get_mut(&target_id)
            .and_then(|e| e.session_state.take())
            .unwrap_or_else(|| {
                crate::state::SessionState::new(self.config.model.clone(), AgentMode::Agent)
            });
        self.session = target_session_state;

        // Update active session ID.
        self.sessions.active_id = target_id.clone();

        // Swap agent tx (we keep background tasks running; just update which tx we use).
        let target_tx = self
            .sessions
            .get(&target_id)
            .and_then(|e| e.agent_tx.clone());
        let target_cancel = self
            .sessions
            .get(&target_id)
            .map(|e| e.agent_cancel.clone())
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));
        let target_busy = self
            .sessions
            .get(&target_id)
            .map(|e| e.busy)
            .unwrap_or(false);

        // If the target session has no agent yet, spawn one.
        if target_tx.is_none() {
            self.spawn_agent_for_active_session().await;
            // New agent is idle; clear busy state so the chat list doesn't show a
            // spinner on the session we just switched to (avoids ghost spinner when
            // clicking list items while another session's change was in progress).
            // Still restore token metrics from the target session.
            self.agent.busy = false;
            self.agent.current_tool = None;
            if let Some(entry) = self.sessions.get(&target_id) {
                self.agent.total_context_tokens = entry.total_context_tokens;
                self.agent.total_context_pct = entry.total_context_pct;
                self.agent.total_output_tokens = entry.total_output_tokens;
                self.agent.cache_hit_pct = entry.cache_hit_pct;
                self.agent.context_pct = entry.context_pct;
                self.agent.current_tool = entry.current_tool.clone();
            }
        } else {
            self.agent.tx = target_tx;
            self.agent.cancel = target_cancel;
            self.agent.busy = target_busy;
            // Restore token-related fields from the target session.
            if let Some(entry) = self.sessions.get(&target_id) {
                self.agent.total_context_tokens = entry.total_context_tokens;
                self.agent.total_context_pct = entry.total_context_pct;
                self.agent.total_output_tokens = entry.total_output_tokens;
                self.agent.cache_hit_pct = entry.cache_hit_pct;
            }
        }

        // Update chat title, yaml path, and jsonl path.
        self.chat_title = self
            .sessions
            .get(&target_id)
            .map(|e| e.title.clone())
            .unwrap_or_else(|| "Chat".to_string());
        self.yaml_path = self
            .sessions
            .get(&target_id)
            .and_then(|e| e.yaml_path.clone());
        self.jsonl_path = self
            .sessions
            .get(&target_id)
            .and_then(|e| e.jsonl_path.clone());

        // Cancel any in-progress inline edit so stale edit state doesn't bleed
        // into the newly active session.
        self.edit.clear();

        // Restore the target session's input buffer and queue (or reset to empty).
        let (target_input_buffer, target_input_cursor, target_input_attachments, target_queue) =
            if let Some(entry) = self.sessions.get_mut(&target_id) {
                (
                    entry.stored_input_buffer.take().unwrap_or_default(),
                    entry.stored_input_cursor.take().unwrap_or(0),
                    entry.stored_input_attachments.take().unwrap_or_default(),
                    entry.stored_queue.take().unwrap_or_else(QueueState::new),
                )
            } else {
                (String::new(), 0, Vec::new(), QueueState::new())
            };
        self.input.buffer = target_input_buffer;
        self.input.cursor = target_input_cursor;
        self.input.scroll_offset = 0;
        self.input.attachments = target_input_attachments;
        self.queue = target_queue;

        self.sessions.sync_list_selection_to_active();
        self.rerender_chat().await;
        self.scroll_to_bottom();
    }

    /// Snapshot the current active session's chat state into its SessionEntry.
    fn save_active_to_session_entry(&mut self) {
        let active_id = self.sessions.active_id.clone();
        if let Some(entry) = self.sessions.get_mut(&active_id) {
            entry.stored_chat = Some(self.chat.clone());
            entry.stored_input_buffer = Some(self.input.buffer.clone());
            entry.stored_input_cursor = Some(self.input.cursor);
            entry.stored_input_attachments = Some(self.input.attachments.clone());
            entry.stored_queue = Some(self.queue.clone());
            entry.session_state = Some(self.session.clone());
            entry.jsonl_path = self.jsonl_path.clone();
            entry.yaml_path = self.yaml_path.clone();
            entry.busy = self.agent.busy;
            entry.current_tool = self.agent.current_tool.clone();
            entry.title = self.chat_title.clone();
            entry.context_pct = self.agent.context_pct;
            entry.total_context_tokens = self.agent.total_context_tokens;
            entry.total_context_pct = self.agent.total_context_pct;
            entry.total_output_tokens = self.agent.total_output_tokens;
            entry.cache_hit_pct = self.agent.cache_hit_pct;
            entry.updated_at = chrono::Utc::now();
        }
    }

    /// Spawn a new local agent task for the currently active session.
    async fn spawn_agent_for_active_session(&mut self) {
        let id = self.sessions.active_id.clone();
        self.spawn_agent_for_session(&id).await;
    }

    /// Spawn a new local agent task for the given session ID, updating
    /// `self.agent` (if it's the active session) and the entry's `agent_tx`.
    async fn spawn_agent_for_session(&mut self, id: &sven_input::SessionId) {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (evt_tx, evt_rx) = mpsc::channel::<AgentEvent>(512);
        // Reuse the shared question sender so all sessions route questions
        // through the single question_rx in run().  Fall back to a disconnected
        // channel only in tests where run() was never called.
        let question_tx = self.question_tx.clone().unwrap_or_else(|| {
            let (tx, _) = mpsc::channel::<sven_tools::QuestionRequest>(4);
            tx
        });
        let cancel = Arc::new(tokio::sync::Mutex::new(None));

        if *id == self.sessions.active_id {
            self.agent.tx = Some(submit_tx.clone());
            self.agent.cancel = cancel.clone();
        }
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.agent_tx = Some(submit_tx);
            entry.agent_cancel = cancel.clone();
        }

        // Forwarding task.
        let mux_tx = self.sessions.multi_event_tx.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            let mut rx = evt_rx;
            while let Some(event) = rx.recv().await {
                if mux_tx.send((session_id.clone(), event)).await.is_err() {
                    break;
                }
            }
        });

        let cfg = self.config.clone();
        let mode = self.session.mode;
        let startup_model_cfg = self.session.model_cfg.clone();
        let shared_skills = self.shared_skills.clone();
        let shared_agents = self.shared_agents.clone();
        let shared_tools = self.shared_tools.clone();
        let shared_tool_displays = self.shared_tool_displays.clone();
        let buffer_store = Arc::clone(&self.buffer_store);

        tokio::spawn(crate::agent::agent_task(
            cfg,
            startup_model_cfg,
            mode,
            submit_rx,
            evt_tx,
            question_tx,
            cancel,
            shared_skills,
            shared_agents,
            shared_tools,
            shared_tool_displays,
            buffer_store,
        ));
    }

    async fn nvim_notify_future(notify: Option<&tokio::sync::Notify>) {
        match notify {
            Some(n) => n.notified().await,
            None => std::future::pending().await,
        }
    }

    // ── Chat list scroll offset ───────────────────────────────────────────────

    /// Compute the scroll offset that was used (or will be used) to render the
    /// chat list, so that visual row → item index conversions are consistent
    /// between click handling and rendering.
    ///
    /// When the pane is focused the last inner row is reserved for the "[enter]
    /// hint", reducing the number of visible items by 1.
    pub(crate) fn chat_list_scroll_offset(&self) -> usize {
        let cl = self.layout.chat_list_pane;
        let focused = self.ui.focus == FocusPane::ChatList;
        let hint_rows = if focused && cl.height >= 3 { 1usize } else { 0 };
        let visible = (cl.height as usize).saturating_sub(2 + hint_rows);
        if self.sessions.list_selected >= visible {
            self.sessions.list_selected + 1 - visible
        } else {
            0
        }
    }

    /// Compute scroll offset for the peers pane.
    pub(crate) fn peers_scroll_offset(&self) -> usize {
        let pp = self.layout.peers_pane;
        let focused = self.ui.focus == FocusPane::Peers;
        let hint_rows = if focused && pp.height >= 3 { 1usize } else { 0 };
        let visible = (pp.height as usize).saturating_sub(2 + hint_rows);
        let selected = self.ui.peers_selected;
        if selected >= visible {
            selected + 1 - visible
        } else {
            0
        }
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

#[cfg(test)]
impl App {
    pub fn for_testing() -> (Self, tokio::sync::mpsc::Receiver<AgentRequest>) {
        let config = Arc::new(sven_config::Config::default());
        let opts = AppOptions {
            mode: sven_config::AgentMode::Agent,
            initial_prompt: None,
            initial_history: None,
            no_nvim: true,
            model_override: None,
            jsonl_path: None,
            jsonl_load_path: None,
            initial_queue: Vec::new(),
            node_backend: None,
            chat_path: None,
            output_chat_path: None,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut app = Self::new(config, opts);
        app.agent.tx = Some(tx);
        (app, rx)
    }

    pub fn inject_input(&mut self, text: &str) {
        self.input.buffer = text.to_string();
        self.input.cursor = text.len();
    }

    pub async fn dispatch_action(&mut self, action: Action) -> bool {
        self.dispatch(action).await
    }

    pub fn is_agent_busy(&self) -> bool {
        self.agent.busy
    }

    pub fn queued_len(&self) -> usize {
        self.queue.messages.len()
    }

    pub fn model_display(&self) -> &str {
        &self.session.model_display
    }

    pub fn simulate_turn_complete(&mut self) {
        self.agent.busy = false;
    }

    pub fn inject_chat_user_message(&mut self, text: &str) -> usize {
        let idx = self.chat.segments.len();
        self.chat
            .segments
            .push(crate::chat::segment::ChatSegment::Message(
                sven_model::Message::user(text),
            ));
        idx
    }

    pub fn start_editing_segment(&mut self, seg_idx: usize, new_text: &str) {
        self.edit.message_index = Some(seg_idx);
        self.edit.buffer = new_text.to_string();
        self.edit.cursor = new_text.len();
        self.edit.original_text = Some(new_text.to_string());
        self.ui.focus = FocusPane::Input;
    }

    pub fn is_abort_pending(&self) -> bool {
        self.queue.abort_pending
    }

    pub async fn simulate_aborted(&mut self, partial_text: &str) {
        use crate::chat::segment::ChatSegment;
        use sven_model::Message;

        self.chat.streaming_buffer.clear();
        self.chat.streaming_is_thinking = false;
        if !partial_text.is_empty() {
            self.chat
                .segments
                .push(ChatSegment::Message(Message::assistant(partial_text)));
        }
        self.agent.busy = false;
        self.agent.current_tool = None;
    }
}
