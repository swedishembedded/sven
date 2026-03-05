// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Top-level TUI application state and event loop.

pub(crate) mod agent_conn;
pub(crate) mod agent_events;
pub(crate) mod chat_ops;
pub(crate) mod chat_state;
pub(crate) mod dispatch;
pub(crate) mod input_state;
pub(crate) mod layout_cache;
pub(crate) mod nvim_state;
pub(crate) mod queue_state;
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
        input_cursor_screen_pos, nvim_cursor_screen_pos, open_pane_block, ChatLabels, ChatPane,
        CompletionMenu, ConfirmModalView, HelpOverlay, InputEditMode, InputPane, QuestionModalView,
        QueueItem, QueuePanel, SearchBar, StatusBar, ToastStack, WelcomeScreen, WhichKeyOverlay,
    },
};

pub(crate) use agent_conn::AgentConn;
pub(crate) use chat_state::ChatState;
pub(crate) use input_state::{EditState, InputState};
pub(crate) use layout_cache::LayoutCache;
pub(crate) use nvim_state::NvimState;
pub(crate) use queue_state::QueueState;
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
    pub(crate) session: crate::state::SessionState,
    pub(crate) command_registry: Arc<CommandRegistry>,
    pub(crate) completion_manager: CompletionManager,
    pub(crate) shared_skills: sven_runtime::SharedSkills,
    pub(crate) shared_agents: sven_runtime::SharedAgents,
    pub(crate) history_path: Option<PathBuf>,
    pub(crate) jsonl_path: Option<PathBuf>,
    /// Set to `true` after a tool call completes — triggers a terminal-state
    /// recovery pass before the next draw.
    pub(crate) needs_terminal_recover: bool,

    // ── Grouped sub-state ─────────────────────────────────────────────────────
    pub(crate) chat: ChatState,
    pub(crate) input: InputState,
    pub(crate) edit: EditState,
    pub(crate) queue: QueueState,
    pub(crate) ui: UiState,
    pub(crate) agent: AgentConn,
    pub(crate) nvim: NvimState,
    pub(crate) layout: LayoutCache,
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

        let mut chat = ChatState::new();
        chat.segments = initial_segments;

        let is_node_proxy = opts.node_backend.is_some();
        let mut app = Self {
            config,
            node_backend: opts.node_backend,
            is_node_proxy,
            session: crate::state::SessionState::new(initial_model_cfg, opts.mode),
            command_registry: registry,
            completion_manager,
            shared_skills,
            shared_agents,
            history_path,
            jsonl_path,
            needs_terminal_recover: false,
            chat,
            input: InputState::new(),
            edit: EditState::new(),
            queue: QueueState::new(),
            ui: UiState::new(),
            agent: AgentConn::new(),
            nvim: NvimState::new(opts.no_nvim),
            layout: LayoutCache::new(),
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

        let layout = AppLayout::new(
            frame,
            self.ui.search.active,
            self.queue.messages.len(),
            self.layout.input_height_pref,
        );
        // Clean up expired toasts every frame.
        self.ui.prune_toasts();

        // ── Status bar ────────────────────────────────────────────────────────
        // In node-proxy mode the node owns model/mode; show "node" as the
        // model label and suppress any staged-override hints.
        let (status_model_name, status_pending_model, status_pending_mode) = if self.is_node_proxy {
            ("node", None, None)
        } else {
            let pending = self.session.staged_model_label().as_deref().map(|s| {
                // SAFETY: model_display lives in session which lives in self
                // which outlives this frame closure.
                unsafe { std::mem::transmute::<&str, &str>(s) }
            });
            (
                &self.session.model_display as &str,
                pending,
                self.session.staged_mode,
            )
        };
        let in_edit = self.edit.active();
        frame.render_widget(
            StatusBar {
                model_name: status_model_name,
                mode: self.session.mode,
                context_pct: self.agent.context_pct,
                cache_hit_pct: self.agent.cache_hit_pct,
                agent_busy: self.agent.busy,
                current_tool: self.agent.current_tool.as_deref(),
                pending_model: status_pending_model,
                pending_mode: status_pending_mode,
                ascii,
                focus: self.ui.focus,
                spinner_frame: self.agent.spinner_frame,
                streaming_tokens: self.agent.streaming_tokens,
                in_edit,
                in_search: self.ui.search.active,
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
                    labels: &ChatLabels {
                        edit_label_lines: self.chat.edit_labels.clone(),
                        remove_label_lines: self.chat.remove_labels.clone(),
                        rerun_label_lines: self.chat.rerun_labels.clone(),
                        copy_label_lines: self.chat.copy_labels.clone(),
                        pending_delete_line: None,
                    },
                    no_nvim: self.nvim.disabled,
                    segment_count: self.chat.segments.len(),
                    auto_scroll_paused,
                    selection: self.chat.normalized_selection(),
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
                        let ModelDirective::SwitchTo(c) = d;
                        // Leak to 'static for the widget lifetime — the queue
                        // lives in self which outlives this frame.
                        Box::leak(format!("{}/{}", c.provider, c.name).into_boxed_str()) as &str
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

        self.agent.tx = Some(submit_tx.clone());
        self.agent.event_rx = Some(event_rx);

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
                )
                .await;
            });
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
                    self.layout.input_height_pref,
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
                    self.layout.input_height_pref,
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
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.ui.search.active,
                    self.queue.messages.len(),
                    self.layout.input_height_pref,
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
                Some(agent_event) = self.recv_agent_event() => {
                    if self.handle_agent_event(agent_event).await { break; }
                }
                Some(Ok(term_event)) = crossterm_events.next() => {
                    if self.handle_term_event(term_event).await { break; }
                }
                Some(req) = question_rx.recv() => {
                    self.handle_question_request(req);
                }
                _ = anim_tick.tick(), if self.agent.busy => {
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

        Ok(())
    }

    pub(crate) async fn recv_agent_event(&mut self) -> Option<AgentEvent> {
        if let Some(rx) = &mut self.agent.event_rx {
            rx.recv().await
        } else {
            None
        }
    }

    async fn nvim_notify_future(notify: Option<&tokio::sync::Notify>) {
        match notify {
            Some(n) => n.notified().await,
            None => std::future::pending().await,
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
