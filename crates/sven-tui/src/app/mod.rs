// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Top-level TUI application state and event loop.

pub(crate) mod agent_events;
pub(crate) mod chat_ops;
pub(crate) mod dispatch;
pub(crate) mod term_events;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

// resolve_auto_log_path is provided by sven-runtime and re-exported below.
use sven_runtime::resolve_auto_log_path;

use crossterm::event::EventStream;
use futures::StreamExt;
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_model::{Message, MessageContent, Role};
use sven_tools::QuestionRequest;
use tokio::sync::mpsc;
use tracing::debug;

use crate::{
    agent::{agent_task, AgentRequest},
    chat::{
        search::SearchState,
        segment::ChatSegment,
    },
    commands::{CommandRegistry, CompletionManager},
    keys::Action,
    layout::AppLayout,
    markdown::StyledLines,
    nvim::NvimBridge,
    overlay::{completion::CompletionOverlay, question::QuestionModal},
    pager::PagerOverlay,
    state::SessionState,
    widgets::{draw_chat, draw_completion_overlay, draw_help, draw_input, draw_question_modal, draw_queue_panel, draw_search, draw_status, InputEditMode},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// Specifies a model switch to take effect with a queued message.
///
/// Wraps a pre-resolved [`ModelConfig`] (resolved by the TUI via
/// [`crate::state::SessionState`]).  The agent task calls
/// `sven_model::from_config` to instantiate the provider.
///
/// The `Unchanged` variant exists for future use (e.g. explicit "use the
/// current model" marker).  `None` in `QueuedMessage::model_transition`
/// means the same thing today.
#[derive(Debug, Clone)]
pub enum ModelDirective {
    /// Keep the current model — reserved for future explicit "no change" use.
    #[allow(dead_code)]
    Unchanged,
    /// Switch to the given pre-resolved model configuration.
    SwitchTo(ModelConfig),
}

impl ModelDirective {
    /// Unwrap the inner [`ModelConfig`] from `SwitchTo`.
    pub fn into_model_config(self) -> ModelConfig {
        match self {
            ModelDirective::Unchanged => ModelConfig::default(),
            ModelDirective::SwitchTo(c) => c,
        }
    }
}

/// A message waiting in the queue, with optional per-message transitions.
///
/// `model_transition` carries a [`ModelDirective`] (resolved by the TUI via
/// `SessionState`).  The agent task calls `sven_model::from_config` to
/// instantiate the provider but no longer performs its own model resolution.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub content: String,
    /// Model to switch to before processing this message, or `None` to keep current.
    pub model_transition: Option<ModelDirective>,
    /// Agent mode to switch to before processing this message, or `None` to keep current.
    pub mode_transition: Option<AgentMode>,
}

impl QueuedMessage {
    pub fn plain(content: String) -> Self {
        Self { content, model_transition: None, mode_transition: None }
    }
}

/// Options passed when constructing the TUI app.
pub struct AppOptions {
    pub mode: AgentMode,
    pub initial_prompt: Option<String>,
    /// Pre-loaded conversation history (from `--resume`).  When set the
    /// segments are injected into the chat pane on startup and the path is
    /// used for subsequent auto-saves.
    pub initial_history: Option<(Vec<ChatSegment>, PathBuf)>,
    /// If true, do not spawn embedded Neovim; use ratatui-only chat view.
    pub no_nvim: bool,
    /// Optional model override from `--model` CLI flag.
    /// Supports the same syntax as the CI runner: `"provider/name"`,
    /// bare provider id, bare model name, or a key defined in `config.providers`.
    pub model_override: Option<String>,
    /// JSONL output path: if set, the conversation is written to this file
    /// after every turn.  When `jsonl_load_path` is also set, history is first
    /// loaded from `jsonl_load_path` and then new turns are appended here.
    pub jsonl_path: Option<PathBuf>,
    /// JSONL input path: if set, the existing conversation is loaded from this
    /// file on startup and seeds the agent history.  May differ from `jsonl_path`
    /// when `--load-jsonl` and `--output-jsonl` point to different files.
    pub jsonl_load_path: Option<PathBuf>,
    /// Initial message queue populated from a `-f workflow.md` file in TUI mode.
    /// Messages are pushed into the queue in order so the user can review and
    /// edit them before they are sent.
    pub initial_queue: Vec<QueuedMessage>,
}

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
    /// The compact queue panel shown above the input when there are pending messages.
    Queue,
}

// ── App ───────────────────────────────────────────────────────────────────────

    /// The top-level TUI application state.
pub struct App {
    pub(crate) config: Arc<Config>,
    /// Unified model/mode session state.  Replaces five previously parallel
    /// fields: `effective_model_name`, `effective_model_cfg`, `model_override`,
    /// `pending_model_override`, `pending_model_display`, plus `mode` and
    /// `pending_mode_override`.
    pub(crate) session: SessionState,
    pub(crate) focus: FocusPane,
    pub(crate) chat_lines: StyledLines,
    /// Structured segments (messages + context-compacted notes).
    /// Source of truth for display and resubmit.
    pub(crate) chat_segments: Vec<ChatSegment>,
    /// Accumulated assistant text during streaming until `TextComplete`.
    pub(crate) streaming_assistant_buffer: String,
    /// True when the streaming buffer is receiving `ThinkingDelta` events
    /// rather than `TextDelta` events.  Controls how the buffer is rendered
    /// in the chat pane while the turn is in progress.
    pub(crate) streaming_is_thinking: bool,
    /// For each segment index: `(start_line, end_line)` in `chat_lines`.
    /// Rebuilt whenever `build_display_from_segments` runs.
    pub(crate) segment_line_ranges: Vec<(usize, usize)>,
    pub(crate) scroll_offset: u16,
    pub(crate) input_buffer: String,
    pub(crate) input_cursor: usize,
    /// Scroll offset for the input box (index of the first visible wrapped line).
    pub(crate) input_scroll_offset: usize,
    /// Scroll offset for the edit box (used in inline edit mode).
    pub(crate) edit_scroll_offset: usize,
    /// Last known inner dimensions of the input pane (content area, sans
    /// border).  Populated each frame from `terminal.size()` so that
    /// `adjust_input_scroll` / `adjust_edit_scroll` can run during event
    /// handling without needing a frame reference.
    pub(crate) last_input_inner_width: u16,
    pub(crate) last_input_inner_height: u16,
    /// Last known inner width of the chat pane (sans border).  Used by
    /// `build_display_from_segments` to pre-wrap content to the exact
    /// available width so that Ratatui does not need a second wrap pass.
    pub(crate) last_chat_inner_width: u16,
    pub(crate) queued: VecDeque<QueuedMessage>,
    /// Slash command registry (built-in + future MCP/skill discovery).
    pub(crate) command_registry: Arc<CommandRegistry>,
    /// Fuzzy completion manager backed by the command registry.
    pub(crate) completion_manager: CompletionManager,
    /// Active completion overlay.  `None` when not in command-completion mode.
    pub(crate) completion_overlay: Option<CompletionOverlay>,
    pub(crate) search: SearchState,
    pub(crate) show_help: bool,
    pub(crate) agent_busy: bool,
    /// Name of the tool currently executing (shown in status bar).
    pub(crate) current_tool: Option<String>,
    pub(crate) context_pct: u8,
    /// Cache hit rate for the last turn (0–100), shown when > 0.
    pub(crate) cache_hit_pct: u8,
    pub(crate) agent_tx: Option<mpsc::Sender<AgentRequest>>,
    pub(crate) event_rx: Option<mpsc::Receiver<AgentEvent>>,
    /// Shared cancel handle: holds the sender half of the current submission's
    /// oneshot channel.  Dropping or sending on it cancels the running turn.
    pub(crate) cancel_handle:
        Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// When `true` after an abort, new messages typed in the input box are
    /// queued rather than sent directly, and auto-dequeue is suppressed.
    /// Cleared when the user manually submits a message.
    pub(crate) abort_pending: bool,
    pub(crate) pending_nav: bool,
    pub(crate) chat_height: u16,
    /// Full-screen pager overlay (Ctrl+T).
    pub(crate) pager: Option<PagerOverlay>,
    /// Active ask-question modal.
    pub(crate) question_modal: Option<QuestionModal>,
    /// `call_id → tool_name` lookup used when rendering tool results.
    pub(crate) tool_args_cache: HashMap<String, String>,
    /// Segment indices that are collapsed (ratatui-only mode).
    pub(crate) collapsed_segments: std::collections::HashSet<usize>,
    /// When set, we are in inline edit mode (editing a chat segment).
    pub(crate) editing_message_index: Option<usize>,
    /// When set, the queue item at this index is being edited in the input box.
    /// Dequeueing is paused while this is `Some`.
    pub(crate) editing_queue_index: Option<usize>,
    /// Which queue item is keyboard-selected in the queue panel.
    pub(crate) queue_selected: Option<usize>,
    /// Last known rect of the queue panel (populated each frame).  Used by the
    /// mouse handler to detect clicks on the queue panel.
    pub(crate) last_queue_pane: Rect,
    pub(crate) edit_buffer: String,
    pub(crate) edit_cursor: usize,
    /// Original text saved for cancel/restore.
    pub(crate) edit_original_text: Option<String>,
    pub(crate) nvim_bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
    pub(crate) nvim_flush_notify:  Option<Arc<tokio::sync::Notify>>,
    pub(crate) nvim_submit_notify: Option<Arc<tokio::sync::Notify>>,
    pub(crate) nvim_quit_notify:   Option<Arc<tokio::sync::Notify>>,
    pub(crate) history_path: Option<PathBuf>,
    /// When set, the full conversation is written to this JSONL file after
    /// every turn and every message edit.
    pub(crate) jsonl_path: Option<PathBuf>,
    pub(crate) no_nvim: bool,
    /// When `true`, new content from the agent automatically scrolls the chat
    /// pane to the bottom.  Set to `false` when the user manually scrolls up
    /// so that streaming does not fight the user's scroll position.
    pub(crate) auto_scroll: bool,
    /// Last known rect of the input pane (populated each frame).  Used by the
    /// mouse handler to route scroll-wheel events to the correct pane.
    pub(crate) last_input_pane: Rect,
    /// Set to `true` after a tool call completes, prompting the run-loop to
    /// verify and restore terminal state (raw mode, cursor visibility) before
    /// the next draw.  Subprocesses that are not fully isolated may alter
    /// terminal settings; this flag triggers a lightweight recovery pass.
    pub(crate) needs_terminal_recover: bool,
}

impl App {
    pub fn new(config: Arc<Config>, opts: AppOptions) -> Self {
        let (initial_segments, history_path) = opts
            .initial_history
            .map(|(segs, path)| (segs, Some(path)))
            .unwrap_or_else(|| (Vec::new(), None));

        // Override initial segments from the JSONL conversation file when given.
        // Uses the full-fidelity format which restores thinking blocks,
        // context-compaction notes, and all message types.
        let initial_segments = if let Some(ref jsonl) = opts.jsonl_path {
            if jsonl.exists() {
                match std::fs::read_to_string(jsonl) {
                    Ok(content) => match sven_input::parse_jsonl_full(&content) {
                        Ok(parsed) => parsed
                            .records
                            .into_iter()
                            .filter_map(|r| match r {
                                sven_input::ConversationRecord::Message(m) => {
                                    // Skip system messages — agent re-injects its own.
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

        // Resolve the effective model config from the CLI override (if any).
        let initial_model_cfg = if let Some(ref mo) = opts.model_override {
            sven_model::resolve_model_from_config(&config, mo)
        } else {
            config.model.clone()
        };

        let registry = Arc::new(CommandRegistry::with_builtins());
        let completion_manager = CompletionManager::new(registry.clone());

        let mut app = Self {
            config,
            session: SessionState::new(initial_model_cfg, opts.mode),
            focus: FocusPane::Input,
            chat_lines: Vec::new(),
            chat_segments: initial_segments,
            streaming_assistant_buffer: String::new(),
            streaming_is_thinking: false,
            segment_line_ranges: Vec::new(),
            scroll_offset: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            input_scroll_offset: 0,
            edit_scroll_offset: 0,
            // Reasonable defaults before the first frame is drawn.
            last_input_inner_width: 78,
            last_input_inner_height: 3,
            last_chat_inner_width: 78,
            queued: VecDeque::new(),
            command_registry: registry,
            completion_manager,
            completion_overlay: None,
            search: SearchState::default(),
            show_help: false,
            agent_busy: false,
            current_tool: None,
            context_pct: 0,
            cache_hit_pct: 0,
            agent_tx: None,
            event_rx: None,
            cancel_handle: Arc::new(tokio::sync::Mutex::new(None)),
            abort_pending: false,
            pending_nav: false,
            chat_height: 24,
            pager: None,
            question_modal: None,
            tool_args_cache: HashMap::new(),
            collapsed_segments: std::collections::HashSet::new(),
            editing_message_index: None,
            editing_queue_index: None,
            queue_selected: None,
            last_queue_pane: Rect::default(),
            edit_buffer: String::new(),
            edit_cursor: 0,
            edit_original_text: None,
            nvim_bridge: None,
            nvim_flush_notify: None,
            nvim_submit_notify: None,
            nvim_quit_notify: None,
            history_path,
            jsonl_path: opts.jsonl_path
                .or_else(|| opts.jsonl_load_path.clone())
                .or_else(resolve_auto_log_path),
            no_nvim: opts.no_nvim,
            auto_scroll: true,
            last_input_pane: Rect::default(),
            needs_terminal_recover: false,
        };
        // Seed the message queue with initial workflow steps (from --file in TUI mode).
        for qm in opts.initial_queue {
            app.queued.push_back(qm);
        }
        if let Some(prompt) = opts.initial_prompt {
            app.queued.push_back(QueuedMessage::plain(prompt));
        }
        // In ratatui-only mode, pre-collapse tool call/result/thinking segments
        // loaded from history so the conversation starts compact.
        if app.no_nvim {
            for (i, seg) in app.chat_segments.iter().enumerate() {
                let is_collapsible = match seg {
                    ChatSegment::Message(m) => matches!(
                        (&m.role, &m.content),
                        (Role::Assistant, MessageContent::ToolCall { .. })
                            | (Role::Tool, MessageContent::ToolResult { .. })
                    ),
                    ChatSegment::Thinking { .. } => true,
                    _ => false,
                };
                if is_collapsible {
                    app.collapsed_segments.insert(i);
                }
            }
        }
        app
    }

    /// Run the TUI event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, event_rx)   = mpsc::channel::<AgentEvent>(512);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(4);

        self.agent_tx = Some(submit_tx.clone());
        self.event_rx = Some(event_rx);

        let cfg                = self.config.clone();
        let mode               = self.session.mode;
        let startup_model_cfg  = self.session.model_cfg.clone();
        let cancel_handle_task = self.cancel_handle.clone();
        tokio::spawn(async move {
            agent_task(
                cfg,
                startup_model_cfg,
                mode,
                submit_rx,
                event_tx,
                question_tx,
                cancel_handle_task,
            )
            .await;
        });

        // When resuming, prime the agent with the loaded history.
        if !self.chat_segments.is_empty() {
            let messages: Vec<Message> = self
                .chat_segments
                .iter()
                .filter_map(|seg| {
                    if let ChatSegment::Message(m) = seg { Some(m.clone()) } else { None }
                })
                .collect();
            if !messages.is_empty() {
                let _ = submit_tx.send(AgentRequest::LoadHistory(messages)).await;
            }
            self.rerender_chat().await;
            self.scroll_to_bottom();
        }

        // Initialize NvimBridge unless disabled by --no-nvim.
        if !self.no_nvim {
            let (nvim_width, nvim_height) = if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    false,
                    0,
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
                    self.nvim_flush_notify  = Some(bridge.flush_notify.clone());
                    self.nvim_submit_notify = Some(bridge.submit_notify.clone());
                    self.nvim_quit_notify   = Some(bridge.quit_notify.clone());
                    self.nvim_bridge = Some(Arc::new(tokio::sync::Mutex::new(bridge)));
                }
                Err(e) => {
                    tracing::error!("Failed to spawn Neovim: {}. Chat view will be degraded.", e);
                }
            }

            if self.nvim_bridge.is_some() && !self.chat_segments.is_empty() {
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
        }

        if let Some(qm) = self.queued.pop_front() {
            self.chat_segments.push(ChatSegment::Message(Message::user(&qm.content)));
            self.rerender_chat().await;
            self.send_to_agent(qm).await;
        }

        let mut crossterm_events = EventStream::new();

        loop {
            if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.search.active,
                    self.queued.len(),
                );
                self.chat_height = layout.chat_inner_height().max(1);
                // Track chat pane inner width for pre-wrap in the markdown renderer.
                self.last_chat_inner_width =
                    layout.chat_pane.width.saturating_sub(2).max(20);
                // Track input pane dimensions for scroll adjustment and mouse routing.
                self.last_input_inner_width =
                    layout.input_pane.width.saturating_sub(2);
                self.last_input_inner_height =
                    layout.input_pane.height.saturating_sub(2);
                self.last_input_pane = layout.input_pane;
                self.last_queue_pane = layout.queue_pane;
            }

            // Keep the cursor visible inside the input / edit box before drawing.
            if self.editing_message_index.is_some() {
                self.adjust_edit_scroll();
            } else {
                self.adjust_input_scroll();
            }

            // After a tool call, a subprocess may have left the terminal in a
            // degraded state.  Re-arm the full set of TUI escape sequences:
            //   • raw mode         — some programs call tcsetattr and restore
            //                        cooked mode when they exit
            //   • mouse capture    — programs like JLinkGDBServer open /dev/tty
            //                        directly and send DisableMouseCapture even
            //                        when their stdio is redirected; setsid()
            //                        in the spawn prevents this for most cases
            //                        but we re-arm here as a belt-and-suspenders
            //                        guarantee
            //   • keyboard flags   — same rationale as mouse capture
            // All escape sequences go to stderr which still points to the real
            // terminal at this point in the TUI run-loop (stderr is dup2'd to
            // /dev/null only after startup; the socket is the write end of the
            // original tty fd and remains valid throughout the session).
            // Actually: stderr was redirected to /dev/null. Use stdout instead.
            if self.needs_terminal_recover {
                self.needs_terminal_recover = false;
                use crossterm::{
                    execute,
                    event::{
                        EnableMouseCapture,
                        KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
                    },
                };
                let raw_was_disabled = !crossterm::terminal::is_raw_mode_enabled().unwrap_or(true);
                if raw_was_disabled {
                    let _ = crossterm::terminal::enable_raw_mode();
                    // Force a complete redraw so any garbage written by the
                    // subprocess is overwritten cleanly.
                    let _ = terminal.clear();
                }
                // Always re-arm mouse capture and keyboard enhancement — these
                // are cheap writes and protect against any escape-sequence
                // injection that survived the setsid() defence.
                let _ = execute!(std::io::stdout(), EnableMouseCapture);
                let _ = execute!(
                    std::io::stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    )
                );
            }

            let ascii = self.ascii();

            let (nvim_lines, nvim_draw_scroll, nvim_cursor) =
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let bridge = nvim_bridge.lock().await;
                    let lines  = bridge.render_to_lines(0, bridge.height).await;
                    let cursor = bridge.get_cursor_pos().await;
                    (lines, 0u16, Some(cursor))
                } else {
                    (Vec::new(), self.scroll_offset, None)
                };

            terminal.draw(|frame| {
                if let Some(pager) = &mut self.pager {
                    pager.render(
                        frame,
                        &self.search.matches,
                        self.search.current,
                        &self.search.query,
                        self.search.regex.as_ref(),
                        ascii,
                    );
                    if self.search.active {
                        let area = frame.area();
                        let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                        draw_search(
                            frame, search_area, &self.search.query,
                            self.search.matches.len(), self.search.current,
                        );
                    }
                    return;
                }

                let layout = AppLayout::new(frame, self.search.active, self.queued.len());

                draw_status(
                    frame, layout.status_bar, &self.session.model_display,
                    self.session.mode, self.context_pct, self.cache_hit_pct, self.agent_busy,
                    self.current_tool.as_deref(),
                    self.session.staged_model_label().as_deref(),
                    self.session.staged_mode,
                    ascii,
                );

                let lines_to_draw = if !nvim_lines.is_empty() { &nvim_lines } else { &self.chat_lines };
                draw_chat(
                    frame, layout.chat_pane, lines_to_draw, nvim_draw_scroll,
                    self.focus == FocusPane::Chat, ascii,
                    &self.search.query, &self.search.matches, self.search.current,
                    self.search.regex.as_ref(), nvim_cursor,
                );
                let edit_mode = if self.editing_queue_index.is_some() {
                    InputEditMode::Queue
                } else if self.editing_message_index.is_some() {
                    InputEditMode::Segment
                } else {
                    InputEditMode::Normal
                };
                let in_edit = edit_mode != InputEditMode::Normal;
                draw_input(
                    frame, layout.input_pane,
                    if in_edit { &self.edit_buffer } else { &self.input_buffer },
                    if in_edit { self.edit_cursor } else { self.input_cursor },
                    if in_edit { self.edit_scroll_offset } else { self.input_scroll_offset },
                    self.focus == FocusPane::Input || in_edit,
                    ascii, edit_mode,
                );
                if !self.queued.is_empty() {
                    let queued_items: Vec<(String, Option<String>, Option<AgentMode>)> = self
                        .queued
                        .iter()
                        .map(|qm| (
                            qm.content.clone(),
                            qm.model_transition.as_ref().map(|d| match d {
                                ModelDirective::SwitchTo(c) => format!("{}/{}", c.provider, c.name),
                                ModelDirective::Unchanged => String::new(),
                            }),
                            qm.mode_transition,
                        ))
                        .collect();
                    draw_queue_panel(
                        frame, layout.queue_pane,
                        &queued_items,
                        self.queue_selected,
                        self.editing_queue_index,
                        self.focus == FocusPane::Queue,
                        ascii,
                    );
                }
                if let Some(ref overlay) = self.completion_overlay {
                    draw_completion_overlay(frame, layout.input_pane, overlay, ascii);
                }
                if self.search.active {
                    draw_search(
                        frame, layout.search_bar, &self.search.query,
                        self.search.matches.len(), self.search.current,
                    );
                }
                if self.show_help {
                    draw_help(frame, ascii);
                }
                if let Some(modal) = &self.question_modal {
                    draw_question_modal(
                        frame,
                        &modal.questions,
                        modal.current_q,
                        &modal.selected_options,
                        modal.other_selected,
                        &modal.other_input,
                        modal.other_cursor,
                        ascii,
                    );
                }
            })?;

            let flush_notify_clone  = self.nvim_flush_notify.clone();
            let submit_notify_clone = self.nvim_submit_notify.clone();
            let quit_notify_clone   = self.nvim_quit_notify.clone();
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

    async fn recv_agent_event(&mut self) -> Option<AgentEvent> {
        if let Some(rx) = &mut self.event_rx { rx.recv().await } else { None }
    }

    async fn nvim_notify_future(notify: Option<&tokio::sync::Notify>) {
        match notify {
            Some(n) => n.notified().await,
            None    => std::future::pending().await,
        }
    }

    // Submit path methods live in crate::submit (src/submit.rs).
}

// ── Test helpers ──────────────────────────────────────────────────────────────

#[cfg(test)]
impl App {
    /// Construct a minimal `App` suitable for integration tests.
    ///
    /// The returned receiver is the mock "agent" channel; call `rx.try_recv()`
    /// to assert on messages dispatched by submit actions.
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
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut app = Self::new(config, opts);
        app.agent_tx = Some(tx);
        (app, rx)
    }

    /// Set the input buffer as if the user typed `text`.
    pub fn inject_input(&mut self, text: &str) {
        self.input_buffer = text.to_string();
        self.input_cursor = text.len();
    }

    /// Drive `dispatch()` from tests (it is normally private).
    pub async fn dispatch_action(&mut self, action: Action) -> bool {
        self.dispatch(action).await
    }

    /// Expose `agent_busy` for test assertions.
    pub fn is_agent_busy(&self) -> bool {
        self.agent_busy
    }

    /// Expose queue length for test assertions.
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// Expose session model display for test assertions.
    pub fn model_display(&self) -> &str {
        &self.session.model_display
    }

    /// Simulate turn completion (agent becomes idle again).
    pub fn simulate_turn_complete(&mut self) {
        self.agent_busy = false;
    }

    /// Push a user message segment directly into the chat history.
    /// Returns the segment index.
    pub fn inject_chat_user_message(&mut self, text: &str) -> usize {
        let idx = self.chat_segments.len();
        self.chat_segments.push(crate::chat::segment::ChatSegment::Message(
            sven_model::Message::user(text),
        ));
        idx
    }

    /// Put the app into inline-edit mode for the chat segment at `seg_idx`,
    /// pre-filling `edit_buffer` with `new_text`.
    pub fn start_editing_segment(&mut self, seg_idx: usize, new_text: &str) {
        self.editing_message_index = Some(seg_idx);
        self.edit_buffer = new_text.to_string();
        self.edit_cursor = new_text.len();
        self.edit_original_text = Some(new_text.to_string());
        self.focus = crate::app::FocusPane::Input;
    }

    /// Expose `abort_pending` for test assertions.
    pub fn is_abort_pending(&self) -> bool {
        self.abort_pending
    }

    /// Simulate an `AgentEvent::Aborted` arriving (agent run stopped mid-stream).
    /// `partial_text` is whatever was streamed before the abort.
    pub async fn simulate_aborted(&mut self, partial_text: &str) {
        use crate::chat::segment::ChatSegment;
        use sven_model::Message;

        self.streaming_assistant_buffer.clear();
        self.streaming_is_thinking = false;
        if !partial_text.is_empty() {
            self.chat_segments
                .push(ChatSegment::Message(Message::assistant(partial_text)));
        }
        self.agent_busy = false;
        self.current_tool = None;
    }
}
