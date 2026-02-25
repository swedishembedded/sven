// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Top-level TUI application state and event loop.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
use futures::StreamExt;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::DefaultTerminal;
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::QuestionRequest;
use tokio::sync::mpsc;
use tracing::debug;

use crate::{
    agent::{agent_task, AgentRequest},
    chat::{
        markdown::{
            apply_bar_and_dim, collapsed_preview, format_conversation, format_todos_markdown,
            parse_markdown_to_messages, segment_bar_style, segment_to_markdown,
        },
        search::SearchState,
        segment::{
            messages_for_resubmit, segment_at_line, segment_editable_text,
            ChatSegment,
        },
    },
    commands::{
        parse, CommandContext, CommandRegistry, CompletionManager,
        ParsedCommand,
        completion::CompletionItem,
    },
    input::{is_reserved_key, to_nvim_notation},
    keys::{map_key, Action},
    layout::AppLayout,
    markdown::{render_markdown, StyledLines},
    nvim::NvimBridge,
    overlay::{completion::CompletionOverlay, question::QuestionModal},
    pager::PagerOverlay,
    widgets::{draw_chat, draw_completion_overlay, draw_help, draw_input, draw_question_modal, draw_queue_panel, draw_search, draw_status, InputEditMode},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// A message waiting in the queue, with optional per-message overrides.
///
/// Overrides apply only to the single agent turn when this message is dequeued.
/// They do not persist across turns.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub content: String,
    pub model_override: Option<String>,
    pub mode_override: Option<AgentMode>,
}

impl QueuedMessage {
    pub fn plain(content: String) -> Self {
        Self { content, model_override: None, mode_override: None }
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
    /// JSONL conversation file: if set, the existing conversation is loaded from
    /// this file on startup, and the file is overwritten after every turn or
    /// message edit to keep it in sync with the in-memory conversation.
    pub jsonl_path: Option<PathBuf>,
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
    config: Arc<Config>,
    mode: AgentMode,
    /// Model name shown in the status bar (the *effective* model after any
    /// `--model` override has been applied, formatted as `"provider/name"`).
    effective_model_name: String,
    /// Resolved config for the currently effective model.  Updated whenever
    /// the model is changed via `/model` or `--model`.  Used to populate
    /// `CommandContext` so completion can highlight the active model.
    effective_model_cfg: ModelConfig,
    /// Optional model override forwarded to the agent task.
    model_override: Option<String>,
    focus: FocusPane,
    chat_lines: StyledLines,
    /// Structured segments (messages + context-compacted notes).
    /// Source of truth for display and resubmit.
    chat_segments: Vec<ChatSegment>,
    /// Accumulated assistant text during streaming until `TextComplete`.
    streaming_assistant_buffer: String,
    /// True when the streaming buffer is receiving `ThinkingDelta` events
    /// rather than `TextDelta` events.  Controls how the buffer is rendered
    /// in the chat pane while the turn is in progress.
    streaming_is_thinking: bool,
    /// For each segment index: `(start_line, end_line)` in `chat_lines`.
    /// Rebuilt whenever `build_display_from_segments` runs.
    segment_line_ranges: Vec<(usize, usize)>,
    scroll_offset: u16,
    input_buffer: String,
    input_cursor: usize,
    /// Scroll offset for the input box (index of the first visible wrapped line).
    input_scroll_offset: usize,
    /// Scroll offset for the edit box (used in inline edit mode).
    edit_scroll_offset: usize,
    /// Last known inner dimensions of the input pane (content area, sans
    /// border).  Populated each frame from `terminal.size()` so that
    /// `adjust_input_scroll` / `adjust_edit_scroll` can run during event
    /// handling without needing a frame reference.
    last_input_inner_width: u16,
    last_input_inner_height: u16,
    /// Last known inner width of the chat pane (sans border).  Used by
    /// `build_display_from_segments` to pre-wrap content to the exact
    /// available width so that Ratatui does not need a second wrap pass.
    last_chat_inner_width: u16,
    queued: VecDeque<QueuedMessage>,
    /// Slash command registry (built-in + future MCP/skill discovery).
    command_registry: Arc<CommandRegistry>,
    /// Fuzzy completion manager backed by the command registry.
    completion_manager: CompletionManager,
    /// Active completion overlay.  `None` when not in command-completion mode.
    completion_overlay: Option<CompletionOverlay>,
    /// Model override for the *next* message to be queued.
    pending_model_override: Option<String>,
    /// Mode override for the *next* message to be queued.
    pending_mode_override: Option<AgentMode>,
    search: SearchState,
    show_help: bool,
    agent_busy: bool,
    /// Name of the tool currently executing (shown in status bar).
    current_tool: Option<String>,
    context_pct: u8,
    /// Cache hit rate for the last turn (0–100), shown when > 0.
    cache_hit_pct: u8,
    agent_tx: Option<mpsc::Sender<AgentRequest>>,
    event_rx: Option<mpsc::Receiver<AgentEvent>>,
    pending_nav: bool,
    chat_height: u16,
    /// Full-screen pager overlay (Ctrl+T).
    pager: Option<PagerOverlay>,
    /// Active ask-question modal.
    question_modal: Option<QuestionModal>,
    /// `call_id → tool_name` lookup used when rendering tool results.
    tool_args_cache: HashMap<String, String>,
    /// Segment indices that are collapsed (ratatui-only mode).
    collapsed_segments: std::collections::HashSet<usize>,
    /// When set, we are in inline edit mode (editing a chat segment).
    editing_message_index: Option<usize>,
    /// When set, the queue item at this index is being edited in the input box.
    /// Dequeueing is paused while this is `Some`.
    editing_queue_index: Option<usize>,
    /// Which queue item is keyboard-selected in the queue panel.
    queue_selected: Option<usize>,
    /// Last known rect of the queue panel (populated each frame).  Used by the
    /// mouse handler to detect clicks on the queue panel.
    last_queue_pane: Rect,
    edit_buffer: String,
    edit_cursor: usize,
    /// Original text saved for cancel/restore.
    edit_original_text: Option<String>,
    nvim_bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
    nvim_flush_notify:  Option<Arc<tokio::sync::Notify>>,
    nvim_submit_notify: Option<Arc<tokio::sync::Notify>>,
    nvim_quit_notify:   Option<Arc<tokio::sync::Notify>>,
    history_path: Option<PathBuf>,
    /// When set, the full conversation is written to this JSONL file after
    /// every turn and every message edit.
    jsonl_path: Option<PathBuf>,
    no_nvim: bool,
    /// When `true`, new content from the agent automatically scrolls the chat
    /// pane to the bottom.  Set to `false` when the user manually scrolls up
    /// so that streaming does not fight the user's scroll position.
    auto_scroll: bool,
    /// Last known rect of the input pane (populated each frame).  Used by the
    /// mouse handler to route scroll-wheel events to the correct pane.
    last_input_pane: Rect,
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
                                } => Some(ChatSegment::ContextCompacted {
                                    tokens_before,
                                    tokens_after,
                                }),
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

        // Compute the resolved effective model config and display name.
        let effective_model_cfg = if let Some(ref mo) = opts.model_override {
            sven_model::resolve_model_from_config(&config, mo)
        } else {
            config.model.clone()
        };
        let effective_model_name = format!("{}/{}", effective_model_cfg.provider, effective_model_cfg.name);

        let model_override = opts.model_override;

        let registry = Arc::new(CommandRegistry::with_builtins());
        let completion_manager = CompletionManager::new(registry.clone());

        let mut app = Self {
            config,
            mode: opts.mode,
            effective_model_name,
            effective_model_cfg,
            model_override,
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
            pending_model_override: None,
            pending_mode_override: None,
            search: SearchState::default(),
            show_help: false,
            agent_busy: false,
            current_tool: None,
            context_pct: 0,
            cache_hit_pct: 0,
            agent_tx: None,
            event_rx: None,
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
            jsonl_path: opts.jsonl_path,
            no_nvim: opts.no_nvim,
            auto_scroll: true,
            last_input_pane: Rect::default(),
        };
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

        let cfg            = self.config.clone();
        let mode           = self.mode;
        let model_override = self.model_override.clone();
        tokio::spawn(async move {
            agent_task(cfg, mode, submit_rx, event_tx, question_tx, model_override).await;
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
                    frame, layout.status_bar, &self.effective_model_name,
                    self.mode, self.context_pct, self.cache_hit_pct, self.agent_busy,
                    self.current_tool.as_deref(),
                    self.pending_model_override.as_deref(),
                    self.pending_mode_override,
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
                        .map(|qm| (qm.content.clone(), qm.model_override.clone(), qm.mode_override))
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

    // ── Agent event handler ───────────────────────────────────────────────────

    async fn handle_agent_event(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::TextDelta(delta) => {
                self.streaming_is_thinking = false;
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::TextComplete(full_text) => {
                self.chat_segments.push(ChatSegment::Message(Message::assistant(&full_text)));
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
            }
            AgentEvent::ToolCallStarted(tc) => {
                self.tool_args_cache.insert(tc.id.clone(), tc.name.clone());
                self.current_tool = Some(tc.name.clone());
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Message(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                }));
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallFinished { call_id, output, .. } => {
                self.current_tool = None;
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Message(
                    Message::tool_result(&call_id, &output),
                ));
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
                self.chat_segments.push(ChatSegment::ContextCompacted { tokens_before, tokens_after });
                self.rerender_chat().await;
            }
            AgentEvent::TokenUsage { input, output, cache_read, .. } => {
                let max = 128_000u32;
                self.context_pct = ((input + output) * 100 / max.max(1)).min(100) as u8;
                // Cache hit rate = cached / total_input * 100
                self.cache_hit_pct = if input > 0 && cache_read > 0 {
                    (cache_read * 100 / input).min(100) as u8
                } else {
                    0
                };
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                self.current_tool = None;
                // Clear per-turn cache indicator so it only shows when the
                // provider actively reports cache hits for the current turn.
                self.cache_hit_pct = 0;
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
                self.save_history_async();
                // Only dequeue the next message if no queue item is being edited.
                if self.editing_queue_index.is_none() {
                    if let Some(next) = self.queued.pop_front() {
                        // Shift the selection down by one since the front was removed.
                        self.queue_selected = self.queue_selected
                            .map(|s| s.saturating_sub(1))
                            .filter(|_| !self.queued.is_empty());
                        // If queue is now empty and we were focused on it, return to Input.
                        if self.queued.is_empty() && self.focus == FocusPane::Queue {
                            self.focus = FocusPane::Input;
                        }
                        self.chat_segments.push(ChatSegment::Message(Message::user(&next.content)));
                        self.rerender_chat().await;
                        self.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_to_agent(next).await;
                    }
                }
            }
            AgentEvent::Error(msg) => {
                self.chat_segments.push(ChatSegment::Error(msg.clone()));
                self.rerender_chat().await;
                self.agent_busy = false;
                self.current_tool = None;
            }
            AgentEvent::TodoUpdate(todos) => {
                let todo_md = format_todos_markdown(&todos);
                self.chat_segments.push(ChatSegment::Message(Message::assistant(&todo_md)));
                self.rerender_chat().await;
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.refresh_todo_display().await {
                        tracing::warn!("Failed to refresh todo display: {}", e);
                    }
                }
            }
            AgentEvent::ThinkingDelta(delta) => {
                self.streaming_is_thinking = true;
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
            AgentEvent::ThinkingComplete(content) => {
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Thinking { content });
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
            }
            _ => {}
        }
        false
    }

    // ── Question request handler ──────────────────────────────────────────────

    fn handle_question_request(&mut self, req: QuestionRequest) {
        debug!(id = %req.id, count = req.questions.len(), "question request received");
        self.question_modal = Some(QuestionModal::new(req.questions, req.answer_tx));
        self.focus = FocusPane::Input;
    }

    // ── Terminal event handler ────────────────────────────────────────────────

    async fn handle_term_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if self.show_help {
                    self.show_help = false;
                    return false;
                }
                if self.question_modal.is_some() {
                    return self.handle_modal_key(k);
                }
                if self.pager.is_some() {
                    return self.handle_pager_key(k).await;
                }

                let in_search = self.search.active;
                let in_input  = self.focus == FocusPane::Input;
                let in_queue  = self.focus == FocusPane::Queue;

                if self.focus == FocusPane::Chat
                    && !in_search
                    && !self.pending_nav
                    && self.nvim_bridge.is_some()
                    && !is_reserved_key(&k)
                {
                    if let Some(nvim_key) = to_nvim_notation(&k) {
                        if let Some(nvim_bridge) = &self.nvim_bridge {
                            let mut bridge = nvim_bridge.lock().await;
                            if let Err(e) = bridge.send_input(&nvim_key).await {
                                tracing::error!("Failed to send key to Neovim: {}", e);
                            }
                        }
                        return false;
                    }
                }

                // When the completion overlay is visible and the input pane
                // has focus, intercept navigation and accept/dismiss keys
                // before they reach the normal input handlers.
                if self.completion_overlay.is_some()
                    && in_input
                    && !in_search
                    && !self.pending_nav
                {
                    use crossterm::event::KeyCode;
                    let shift = k.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                    let overlay_action = match k.code {
                        KeyCode::Enter => Some(Action::CompletionSelect),
                        KeyCode::Esc   => Some(Action::CompletionCancel),
                        KeyCode::Down  => Some(Action::CompletionNext),
                        KeyCode::Up    => Some(Action::CompletionPrev),
                        KeyCode::Tab if !shift => Some(Action::CompletionNext),
                        KeyCode::BackTab       => Some(Action::CompletionPrev),
                        _ => None,
                    };
                    if let Some(action) = overlay_action {
                        self.pending_nav = false;
                        return self.dispatch(action).await;
                    }
                }

                let in_edit_mode = self.editing_message_index.is_some()
                    || self.editing_queue_index.is_some();
                if let Some(action) = map_key(k, in_search, in_input, self.pending_nav, in_edit_mode, in_queue) {
                    if action == Action::NavPrefix {
                        self.pending_nav = true;
                        return false;
                    }
                    self.pending_nav = false;
                    return self.dispatch(action).await;
                }
                self.pending_nav = false;
                false
            }

            Event::Mouse(mouse) => {
                if self.pager.is_none() {
                    let over_input = mouse.row >= self.last_input_pane.y
                        && mouse.row < self.last_input_pane.y + self.last_input_pane.height;
                    let over_queue = self.last_queue_pane.height > 0
                        && mouse.row >= self.last_queue_pane.y
                        && mouse.row < self.last_queue_pane.y + self.last_queue_pane.height;
                    let in_edit = self.editing_message_index.is_some() || self.editing_queue_index.is_some();
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if over_input {
                                if in_edit {
                                    self.edit_scroll_offset =
                                        self.edit_scroll_offset.saturating_sub(3);
                                } else {
                                    self.input_scroll_offset =
                                        self.input_scroll_offset.saturating_sub(3);
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-y><C-y><C-y>").await;
                                }
                            } else {
                                self.scroll_up(3);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if over_input {
                                let w = self.last_input_inner_width as usize;
                                let h = self.last_input_inner_height as usize;
                                if w > 0 && h > 0 {
                                    let total = crate::input_wrap::wrap_content(
                                        if in_edit { &self.edit_buffer } else { &self.input_buffer },
                                        w, 0,
                                    ).lines.len();
                                    let max = total.saturating_sub(h);
                                    if in_edit {
                                        self.edit_scroll_offset =
                                            (self.edit_scroll_offset + 3).min(max);
                                    } else {
                                        self.input_scroll_offset =
                                            (self.input_scroll_offset + 3).min(max);
                                    }
                                }
                            } else if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-e><C-e><C-e>").await;
                                }
                            } else {
                                self.scroll_down(3);
                            }
                        }
                        MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            if self.no_nvim =>
                        {
                            // ── Click on queue panel ──────────────────────────────
                            if over_queue && !self.queued.is_empty() {
                                let inner_y = self.last_queue_pane.y + 1; // skip border
                                if mouse.row >= inner_y {
                                    let item_idx = (mouse.row - inner_y) as usize;
                                    if item_idx < self.queued.len() {
                                        self.queue_selected = Some(item_idx);
                                        self.focus = FocusPane::Queue;
                                        // Double-click or single click: open edit
                                        if let Some(qm) = self.queued.get(item_idx) {
                                            let text = qm.content.clone();
                                            self.editing_queue_index = Some(item_idx);
                                            self.edit_cursor = text.len();
                                            self.edit_original_text = Some(text.clone());
                                            self.edit_buffer = text;
                                            self.focus = FocusPane::Input;
                                        }
                                    }
                                }
                            }

                            // ── Click on chat pane ───────────────────────────────
                            let content_start_row: u16 = 2;
                            if mouse.row >= content_start_row && !over_queue && !over_input {
                                let click_line = (mouse.row - content_start_row) as usize
                                    + self.scroll_offset as usize;
                                if let Some(seg_idx) =
                                    segment_at_line(&self.segment_line_ranges, click_line)
                                {
                                    if let Some(seg) = self.chat_segments.get(seg_idx) {
                                        let is_editable =
                                            segment_editable_text(&self.chat_segments, seg_idx).is_some();
                                        if is_editable {
                                            if let Some(text) =
                                                segment_editable_text(&self.chat_segments, seg_idx)
                                            {
                                                self.editing_message_index = Some(seg_idx);
                                                self.edit_cursor = text.len();
                                                self.edit_original_text = Some(text.clone());
                                                self.edit_buffer = text;
                                                self.focus = FocusPane::Input;
                                                self.update_editing_segment_live();
                                                self.rerender_chat().await;
                                            }
                                        } else {
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
                                                if self.collapsed_segments.contains(&seg_idx) {
                                                    self.collapsed_segments.remove(&seg_idx);
                                                } else {
                                                    self.collapsed_segments.insert(seg_idx);
                                                }
                                                self.build_display_from_segments();
                                                self.search.update_matches(&self.chat_lines);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                false
            }

            Event::Resize(width, height) => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let layout = AppLayout::compute(
                        Rect::new(0, 0, width, height),
                        self.search.active,
                        self.queued.len(),
                    );
                    let chat_width  = layout.chat_pane.width.saturating_sub(2);
                    let chat_height = layout.chat_inner_height();
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.resize(chat_width, chat_height).await {
                        tracing::error!("Failed to resize Neovim UI: {}", e);
                    }
                }
                self.rerender_chat().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                false
            }

            _ => false,
        }
    }

    // ── Question modal key handling ───────────────────────────────────────────

    fn handle_modal_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        let modal = match &mut self.question_modal {
            Some(m) => m,
            None => return false,
        };

        match k.code {
            KeyCode::Esc => {
                let modal = self.question_modal.take().unwrap();
                modal.cancel();
            }
            KeyCode::Enter => {
                let done = modal.submit();
                if done {
                    let modal = self.question_modal.take().unwrap();
                    modal.finish();
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                modal.toggle_other();
            }
            KeyCode::Char(c @ '1'..='9') => {
                // Option number (1-indexed from display, 0-indexed internally)
                if let Some(idx) = c.to_digit(10) {
                    let option_idx = idx as usize - 1;
                    if modal.current_q < modal.questions.len() {
                        let q = &modal.questions[modal.current_q];
                        if option_idx < q.options.len() {
                            modal.toggle_option(option_idx);
                        }
                    }
                }
            }
            // Text input for "Other" field when Other is selected
            KeyCode::Char(c) if modal.other_selected => {
                modal.other_input.insert(modal.other_cursor, c);
                modal.other_cursor += c.len_utf8();
            }
            KeyCode::Backspace if modal.other_selected => {
                if modal.other_cursor > 0 {
                    let prev = prev_char_boundary(&modal.other_input, modal.other_cursor);
                    modal.other_input.remove(prev);
                    modal.other_cursor = prev;
                }
            }
            KeyCode::Left if modal.other_selected => {
                modal.other_cursor = prev_char_boundary(&modal.other_input, modal.other_cursor);
            }
            KeyCode::Right if modal.other_selected => {
                if modal.other_cursor < modal.other_input.len() {
                    let ch = modal.other_input[modal.other_cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    modal.other_cursor += ch;
                }
            }
            KeyCode::Home if modal.other_selected => { modal.other_cursor = 0; }
            KeyCode::End if modal.other_selected => { modal.other_cursor = modal.other_input.len(); }
            _ => {}
        }
        false
    }

    // ── Pager key handling ────────────────────────────────────────────────────

    async fn handle_pager_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crate::keys::map_search_key;
        use crate::pager::PagerAction;

        if self.search.active {
            if let Some(action) = map_search_key(k) {
                return self.dispatch(action).await;
            }
            return false;
        }

        let pager = match &mut self.pager {
            Some(p) => p,
            None => return false,
        };

        match pager.handle_key(k) {
            PagerAction::Close => { self.pager = None; }
            PagerAction::OpenSearch => {
                self.search.query.clear();
                self.search.current = 0;
                self.search.update_matches(&self.chat_lines);
                self.search.active = true;
            }
            PagerAction::SearchNext => {
                if !self.search.matches.is_empty() {
                    self.search.current =
                        (self.search.current + 1) % self.search.matches.len();
                    if let Some(line) = self.search.current_line() {
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::SearchPrev => {
                if !self.search.matches.is_empty() {
                    self.search.current = self.search.current
                        .checked_sub(1)
                        .unwrap_or(self.search.matches.len() - 1);
                    if let Some(line) = self.search.current_line() {
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            PagerAction::Handled => {}
        }
        false
    }

    // ── Action dispatcher ─────────────────────────────────────────────────────

    async fn dispatch(&mut self, action: Action) -> bool {
        // Route input-manipulation actions to the edit buffer whenever we are in
        // any edit mode — both chat-segment edits and queue-item edits.
        if self.editing_message_index.is_some() || self.editing_queue_index.is_some() {
            if let Some((buf, cur)) = self.apply_input_to_edit(&action) {
                self.edit_buffer = buf;
                self.edit_cursor = cur;
                // Live-preview only makes sense for chat segments (not queue items).
                if self.editing_message_index.is_some() {
                    self.update_editing_segment_live();
                    self.rerender_chat().await;
                }
                return false;
            }
        }

        match action {
            Action::FocusInput => {
                self.focus = FocusPane::Input;
            }
            Action::NavUp => {
                // Ctrl+w k: move focus upward through visible panes
                match self.focus {
                    FocusPane::Input => {
                        if !self.queued.is_empty() {
                            if self.queue_selected.is_none() {
                                self.queue_selected = Some(0);
                            }
                            self.focus = FocusPane::Queue;
                        } else {
                            self.focus = FocusPane::Chat;
                        }
                    }
                    FocusPane::Queue => {
                        self.focus = FocusPane::Chat;
                    }
                    FocusPane::Chat => {
                        // Already at the top; stay in Chat
                    }
                }
            }
            Action::NavDown => {
                // Ctrl+w j: move focus downward through visible panes
                match self.focus {
                    FocusPane::Chat => {
                        if !self.queued.is_empty() {
                            if self.queue_selected.is_none() {
                                self.queue_selected = Some(0);
                            }
                            self.focus = FocusPane::Queue;
                        } else {
                            self.focus = FocusPane::Input;
                        }
                    }
                    FocusPane::Queue => {
                        self.focus = FocusPane::Input;
                    }
                    FocusPane::Input => {
                        // Already at the bottom; stay in Input
                    }
                }
            }
            Action::FocusQueue => {
                if !self.queued.is_empty() {
                    if self.queue_selected.is_none() {
                        self.queue_selected = Some(0);
                    }
                    self.focus = FocusPane::Queue;
                }
            }
            Action::QueueNavUp => {
                if let Some(sel) = self.queue_selected {
                    self.queue_selected = Some(sel.saturating_sub(1));
                } else if !self.queued.is_empty() {
                    self.queue_selected = Some(0);
                }
            }
            Action::QueueNavDown => {
                let len = self.queued.len();
                if len > 0 {
                    let sel = self.queue_selected.unwrap_or(0);
                    self.queue_selected = Some((sel + 1).min(len - 1));
                }
            }
            Action::QueueEditSelected => {
                if let Some(idx) = self.queue_selected {
                    if let Some(qm) = self.queued.get(idx) {
                        let text = qm.content.clone();
                        self.editing_queue_index = Some(idx);
                        self.edit_cursor = text.len();
                        self.edit_original_text = Some(text.clone());
                        self.edit_buffer = text;
                        self.focus = FocusPane::Input;
                    }
                }
            }

            Action::EditMessageAtCursor => {
                let line = self.scroll_offset as usize;
                if let Some(seg_idx) = segment_at_line(&self.segment_line_ranges, line) {
                    if let Some(text) = segment_editable_text(&self.chat_segments, seg_idx) {
                        self.editing_message_index = Some(seg_idx);
                        self.edit_cursor = text.len();
                        self.edit_original_text = Some(text.clone());
                        self.edit_buffer = text;
                        self.focus = FocusPane::Input;
                    }
                }
            }

            Action::DeleteQueuedMessage => {
                if let Some(idx) = self.queue_selected {
                    if idx < self.queued.len() {
                        // If we were editing this item, cancel the edit first.
                        if self.editing_queue_index == Some(idx) {
                            self.editing_queue_index = None;
                            self.edit_buffer.clear();
                            self.edit_cursor = 0;
                            self.edit_scroll_offset = 0;
                            self.edit_original_text = None;
                        }
                        self.queued.remove(idx);
                        // Keep selection in bounds.
                        if self.queued.is_empty() {
                            self.queue_selected = None;
                            if self.focus == FocusPane::Queue {
                                self.focus = FocusPane::Input;
                            }
                        } else {
                            self.queue_selected = Some(idx.min(self.queued.len() - 1));
                        }
                    }
                }
            }
            Action::EditMessageConfirm => {
                // Handle queue-item edit confirm.
                if let Some(q_idx) = self.editing_queue_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_queue_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
                    self.edit_original_text = None;
                    if !new_content.is_empty() {
                        if let Some(entry) = self.queued.get_mut(q_idx) {
                            entry.content = new_content;
                        }
                    }
                    // Return focus to Queue if it still has items, otherwise Input.
                    self.focus = if self.queued.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    // If the agent finished while we were editing, pick up the queue now.
                    self.try_dequeue_next().await;
                    return false;
                }
                // Handle chat-segment edit confirm.
                if let Some(i) = self.editing_message_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_message_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
                    self.edit_original_text = None;
                    if new_content.is_empty() {
                        return false;
                    }
                    let seg = match self.chat_segments.get(i) {
                        Some(ChatSegment::Message(m)) => m.clone(),
                        _ => return false,
                    };
                    match (&seg.role, &seg.content) {
                        (Role::User, MessageContent::Text(_)) => {
                            self.chat_segments.truncate(i + 1);
                            self.chat_segments.pop();
                            self.chat_segments.push(ChatSegment::Message(Message::user(&new_content)));
                            let messages = messages_for_resubmit(&self.chat_segments);
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, QueuedMessage::plain(new_content)).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(i) {
                                m.content = MessageContent::Text(new_content);
                            }
                            self.build_display_from_segments();
                            self.search.update_matches(&self.chat_lines);
                            self.rerender_chat().await;
                            self.save_history_async();
                        }
                        _ => {}
                    }
                }
            }
            Action::EditMessageCancel => {
                // Cancel queue-item edit — restore original text if available.
                if self.editing_queue_index.is_some() {
                    if let (Some(q_idx), Some(original)) =
                        (self.editing_queue_index, self.edit_original_text.clone())
                    {
                        if let Some(entry) = self.queued.get_mut(q_idx) {
                            entry.content = original;
                        }
                    }
                    self.editing_queue_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
                    self.edit_original_text = None;
                    // Return focus to Queue if it still has items, otherwise Input.
                    self.focus = if self.queued.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    // If the agent finished while we were editing, pick up the queue now.
                    self.try_dequeue_next().await;
                    return false;
                }
                // Cancel chat-segment edit.
                if let Some(idx) = self.editing_message_index {
                    if let Some(original) = self.edit_original_text.clone() {
                        match self.chat_segments.get_mut(idx) {
                            Some(ChatSegment::Message(m)) => {
                                match (&m.role, &mut m.content) {
                                    (Role::User, MessageContent::Text(t)) => { *t = original; }
                                    (Role::Assistant, MessageContent::Text(t)) => { *t = original; }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        self.build_display_from_segments();
                        self.search.update_matches(&self.chat_lines);
                    }
                }
                self.editing_message_index = None;
                self.edit_buffer.clear();
                self.edit_cursor = 0;
                self.edit_scroll_offset = 0;
                self.edit_original_text = None;
            }

            Action::SubmitBufferToAgent => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let markdown = {
                        let bridge = nvim_bridge.lock().await;
                        match bridge.get_buffer_content().await {
                            Ok(content) => content,
                            Err(e) => {
                                tracing::error!("Failed to get buffer content: {}", e);
                                return false;
                            }
                        }
                    };
                    match parse_markdown_to_messages(&markdown) {
                        Ok(messages) => {
                            if messages.is_empty() {
                                tracing::warn!("Empty buffer, nothing to submit");
                                return false;
                            }
                            let new_user_content = messages
                                .iter()
                                .rev()
                                .find(|m| m.role == Role::User)
                                .and_then(|m| m.as_text())
                                .unwrap_or("")
                                .to_string();
                            if new_user_content.is_empty() {
                                tracing::warn!("No user message found in buffer");
                                return false;
                            }
                            self.chat_segments = messages
                                .iter()
                                .map(|m| ChatSegment::Message(m.clone()))
                                .collect();
                            self.tool_args_cache.clear();
                            for msg in &messages {
                                if let MessageContent::ToolCall { tool_call_id, function } = &msg.content {
                                    self.tool_args_cache.insert(tool_call_id.clone(), function.name.clone());
                                }
                            }
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, QueuedMessage::plain(new_user_content)).await;
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse buffer markdown: {}", e);
                            return false;
                        }
                    }
                } else {
                    tracing::warn!("SubmitBufferToAgent called but nvim_bridge not available");
                }
            }

            Action::ScrollUp => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-y>").await;
                } else {
                    self.scroll_up(1);
                }
            }
            Action::ScrollDown => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-e>").await;
                } else {
                    self.scroll_down(1);
                }
            }
            Action::ScrollPageUp => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-u>").await;
                } else {
                    self.scroll_up(self.chat_height / 2);
                }
            }
            Action::ScrollPageDown => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-d>").await;
                } else {
                    self.scroll_down(self.chat_height / 2);
                }
            }
            Action::ScrollTop => {
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("gg").await;
                } else {
                    self.scroll_offset = 0;
                    self.auto_scroll = false;
                }
            }
            Action::ScrollBottom => {
                self.auto_scroll = true;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
            }

            Action::SearchOpen => {
                self.search.query.clear();
                self.search.current = 0;
                self.search.update_matches(&self.chat_lines);
                self.search.active = true;
                self.focus = FocusPane::Chat;
            }
            Action::SearchClose => {
                self.search.active = false;
                if let Some(line) = self.search.current_line() {
                    if let Some(pager) = &mut self.pager {
                        pager.scroll_to_line(line);
                    }
                }
            }
            Action::SearchInput(c) => {
                self.search.query.push(c);
                self.search.update_matches(&self.chat_lines);
                if let Some(line) = self.search.current_line() {
                    self.scroll_offset = line as u16;
                    if let Some(pager) = &mut self.pager {
                        pager.scroll_to_line(line);
                    }
                }
            }
            Action::SearchBackspace => {
                self.search.query.pop();
                self.search.update_matches(&self.chat_lines);
            }
            Action::SearchNextMatch => {
                if !self.search.matches.is_empty() {
                    self.search.current =
                        (self.search.current + 1) % self.search.matches.len();
                    if let Some(line) = self.search.current_line() {
                        self.scroll_offset = line as u16;
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            Action::SearchPrevMatch => {
                if !self.search.matches.is_empty() {
                    self.search.current = self.search.current
                        .checked_sub(1)
                        .unwrap_or(self.search.matches.len() - 1);
                    if let Some(line) = self.search.current_line() {
                        self.scroll_offset = line as u16;
                        if let Some(pager) = &mut self.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }

            Action::InputChar(c) => {
                self.input_buffer.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
                // Auto-trigger / update completion overlay when typing a slash command.
                if self.input_buffer.starts_with('/') {
                    self.update_completion_overlay();
                } else {
                    self.completion_overlay = None;
                }
            }
            Action::InputNewline => {
                self.input_buffer.insert(self.input_cursor, '\n');
                self.input_cursor += 1;
            }
            Action::InputBackspace => {
                if self.input_cursor > 0 {
                    let prev = prev_char_boundary(&self.input_buffer, self.input_cursor);
                    self.input_buffer.remove(prev);
                    self.input_cursor = prev;
                }
                if self.input_buffer.starts_with('/') {
                    self.update_completion_overlay();
                } else {
                    self.completion_overlay = None;
                }
            }
            Action::InputDelete => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_buffer.remove(self.input_cursor);
                }
            }
            Action::InputMoveCursorLeft => {
                self.input_cursor = prev_char_boundary(&self.input_buffer, self.input_cursor);
            }
            Action::InputMoveCursorRight => {
                if self.input_cursor < self.input_buffer.len() {
                    let ch = self.input_buffer[self.input_cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    self.input_cursor += ch;
                }
            }
            Action::InputMoveWordLeft => {
                self.input_cursor = prev_word_boundary(&self.input_buffer, self.input_cursor);
            }
            Action::InputMoveWordRight => {
                self.input_cursor = next_word_boundary(&self.input_buffer, self.input_cursor);
            }
            Action::InputMoveLineStart => self.input_cursor = 0,
            Action::InputMoveLineEnd   => self.input_cursor = self.input_buffer.len(),
            Action::InputMoveLineUp => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&self.input_buffer, w, self.input_cursor);
                    if ws.cursor_row > 0 {
                        self.input_cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input_buffer, w, ws.cursor_row - 1, ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&self.input_buffer, w, self.input_cursor);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        self.input_cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input_buffer, w, ws.cursor_row + 1, ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputPageUp => {
                let h = self.last_input_inner_height as usize;
                if self.editing_message_index.is_some() || self.editing_queue_index.is_some() {
                    self.edit_scroll_offset = self.edit_scroll_offset.saturating_sub(h);
                } else {
                    self.input_scroll_offset = self.input_scroll_offset.saturating_sub(h);
                }
            }
            Action::InputPageDown => {
                let w = self.last_input_inner_width as usize;
                let h = self.last_input_inner_height as usize;
                if w > 0 && h > 0 {
                    let in_edit = self.editing_message_index.is_some() || self.editing_queue_index.is_some();
                    let content = if in_edit { &self.edit_buffer } else { &self.input_buffer };
                    let ws = crate::input_wrap::wrap_content(content, w, 0);
                    let max = ws.lines.len().saturating_sub(h);
                    if in_edit {
                        self.edit_scroll_offset = (self.edit_scroll_offset + h).min(max);
                    } else {
                        self.input_scroll_offset = (self.input_scroll_offset + h).min(max);
                    }
                }
            }
            Action::InputDeleteToEnd   => self.input_buffer.truncate(self.input_cursor),
            Action::InputDeleteToStart => {
                self.input_buffer = self.input_buffer[self.input_cursor..].to_string();
                self.input_cursor = 0;
            }

            Action::Submit => {
                // Dismiss completion overlay when Enter is pressed.
                self.completion_overlay = None;

                let text = std::mem::take(&mut self.input_buffer).trim().to_string();
                self.input_cursor = 0;
                self.input_scroll_offset = 0;

                if text.is_empty() {
                    return false;
                }

                // Parse as slash command.
                let parsed = parse(&text);

                // Handle a fully-complete slash command (or a bare "/name" with no space).
                // Also handles "/name " (CompletingArgs arg 0 empty = command entered, no args).
                let is_slash_command = match &parsed {
                    ParsedCommand::Complete { .. } => true,
                    ParsedCommand::PartialCommand { .. } => true,
                    ParsedCommand::CompletingArgs { arg_index, partial, .. } => {
                        *arg_index == 0 && partial.is_empty()
                    }
                    ParsedCommand::NotCommand => false,
                };
                if is_slash_command {
                    let (cmd_name, cmd_args) = match &parsed {
                        ParsedCommand::Complete { command, args } => (command.as_str(), args.clone()),
                        ParsedCommand::PartialCommand { partial } => (partial.as_str(), vec![]),
                        ParsedCommand::CompletingArgs { command, .. } => (command.as_str(), vec![]),
                        ParsedCommand::NotCommand => unreachable!(),
                    };

                    if let Some(cmd) = self.command_registry.get(cmd_name) {
                        let result = cmd.execute(cmd_args);

                        // Handle immediate actions first (e.g. quit).
                        if let Some(crate::commands::ImmediateAction::Quit) = result.immediate_action {
                            return true;
                        }

                        // Store per-message overrides.
                        if let Some(model) = result.model_override {
                            self.pending_model_override = Some(model);
                            // Resolve and cache the effective model config so
                            // CommandContext (completions) always reflects the
                            // currently-selected model, not the YAML baseline.
                            let resolved = sven_model::resolve_model_from_config(
                                &self.config, self.pending_model_override.as_ref().unwrap()
                            );
                            self.effective_model_cfg = resolved.clone();
                            // Show the pending override in the status bar if not busy.
                            if !self.agent_busy {
                                self.effective_model_name =
                                    format!("{}/{}", resolved.provider, resolved.name);
                            }
                        }
                        if let Some(mode) = result.mode_override {
                            self.pending_mode_override = Some(mode);
                            if !self.agent_busy {
                                self.mode = mode;
                            }
                        }

                        // If the command wants to send a message, use it; otherwise just exit.
                        if result.message_to_send.is_none() {
                            return false;
                        }
                        // Fall through with the command-provided message (not used by built-ins yet).
                    }
                }

                // If the text is a slash command that didn't match a known command name
                // (e.g. just typed "/" or "/unknown"), don't send.
                if text.starts_with('/') {
                    match &parsed {
                        ParsedCommand::PartialCommand { partial } => {
                            if self.command_registry.get(partial.as_str()).is_none() {
                                // Unknown or incomplete command.
                                return false;
                            }
                        }
                        ParsedCommand::CompletingArgs { command, .. } => {
                            // Still completing args — don't send yet.
                            let _ = command;
                            return false;
                        }
                        _ => {}
                    }
                }

                self.auto_scroll = true;
                let qm = QueuedMessage {
                    content: text.clone(),
                    model_override: self.pending_model_override.take(),
                    mode_override: self.pending_mode_override.take(),
                };

                if self.agent_busy {
                    self.queued.push_back(qm);
                    self.queue_selected = Some(self.queued.len() - 1);
                } else {
                    self.sync_nvim_buffer_to_segments().await;
                    let history = messages_for_resubmit(&self.chat_segments);
                    self.chat_segments.push(ChatSegment::Message(Message::user(&text)));
                    self.rerender_chat().await;
                    self.scroll_to_bottom();
                    self.send_resubmit_to_agent(history, qm).await;
                }
            }

            Action::CompletionNext => {
                if let Some(overlay) = &mut self.completion_overlay {
                    overlay.select_next();
                } else if self.input_buffer.starts_with('/') {
                    self.update_completion_overlay();
                }
            }

            Action::CompletionPrev => {
                if let Some(overlay) = &mut self.completion_overlay {
                    overlay.select_prev();
                }
            }

            Action::CompletionSelect => {
                if let Some(overlay) = self.completion_overlay.take() {
                    if let Some(item) = overlay.selected_item() {
                        let item = item.clone();
                        self.apply_completion(&item);
                    }
                }
            }

            Action::CompletionCancel => {
                self.completion_overlay = None;
            }

            Action::InterruptAgent => {
                // TODO: send cancellation signal
            }

            Action::CycleMode => {
                self.mode = match self.mode {
                    AgentMode::Research => AgentMode::Plan,
                    AgentMode::Plan     => AgentMode::Agent,
                    AgentMode::Agent    => AgentMode::Research,
                };
            }

            Action::Help => {
                self.show_help = !self.show_help;
            }

            Action::OpenPager => {
                let mut pager = PagerOverlay::new(self.chat_lines.clone());
                if let Some(line) = self.search.current_line() {
                    pager.scroll_to_line(line);
                }
                self.pager = Some(pager);
            }

            _ => {}
        }
        false
    }

    async fn send_to_agent(&mut self, qm: QueuedMessage) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Submit {
                    content: qm.content,
                    model_override: qm.model_override,
                    mode_override: qm.mode_override,
                })
                .await;
            self.agent_busy = true;
        }
    }

    async fn send_resubmit_to_agent(&mut self, messages: Vec<Message>, qm: QueuedMessage) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Resubmit {
                    messages,
                    new_user_content: qm.content,
                    model_override: qm.model_override,
                    mode_override: qm.mode_override,
                })
                .await;
            self.agent_busy = true;
        }
    }

    /// If the agent is currently idle and there are queued messages waiting,
    /// dequeue the first one and send it.  Called after a queue-item edit ends
    /// so that a turn that completed while the user was editing isn't dropped.
    async fn try_dequeue_next(&mut self) {
        if !self.agent_busy && self.editing_queue_index.is_none() {
            if let Some(next) = self.queued.pop_front() {
                self.queue_selected = self.queue_selected
                    .map(|s| s.saturating_sub(1))
                    .filter(|_| !self.queued.is_empty());
                if self.queued.is_empty() && self.focus == FocusPane::Queue {
                    self.focus = FocusPane::Input;
                }
                self.chat_segments.push(ChatSegment::Message(Message::user(&next.content)));
                self.rerender_chat().await;
                self.auto_scroll = true;
                self.scroll_to_bottom();
                self.send_to_agent(next).await;
            }
        }
    }

    // ── Slash command completion ──────────────────────────────────────────────

    /// Regenerate completions from the current `input_buffer` and update (or
    /// dismiss) the `completion_overlay`.
    fn update_completion_overlay(&mut self) {
        let parsed = parse(&self.input_buffer);
        let ctx = CommandContext {
            config: self.config.clone(),
            current_model_provider: self.effective_model_cfg.provider.clone(),
            current_model_name: self.effective_model_cfg.name.clone(),
        };
        let items = self.completion_manager.get_completions(&parsed, &ctx);
        if items.is_empty() {
            self.completion_overlay = None;
        } else {
            let prev_selected = self.completion_overlay.as_ref().map(|o| o.selected).unwrap_or(0);
            let mut overlay = CompletionOverlay::new(items);
            // Keep the previously-selected index in bounds.
            overlay.selected = prev_selected.min(overlay.items.len().saturating_sub(1));
            overlay.adjust_scroll_pub();
            self.completion_overlay = Some(overlay);
        }
    }

    /// Apply the selected completion item to `input_buffer`.
    ///
    /// Replaces either the command name or the current argument with the
    /// selected value, then positions the cursor appropriately.
    fn apply_completion(&mut self, item: &CompletionItem) {
        let parsed = parse(&self.input_buffer);
        match parsed {
            ParsedCommand::PartialCommand { .. } => {
                // Replace everything after the leading '/' with the command name.
                self.input_buffer = format!("/{} ", item.value.trim_start_matches('/'));
                self.input_cursor = self.input_buffer.len();
            }
            ParsedCommand::CompletingArgs { command, arg_index, partial: _ } => {
                // Build new buffer: "/command arg0 … argN-1 <selected>"
                // We only have arg_index here but we know partial is the last
                // word.  Reconstruct by keeping everything up to the partial.
                let prefix = if arg_index == 0 {
                    format!("/{} ", command)
                } else {
                    // Keep existing args up to arg_index — simple: re-split
                    // We strip the partial from the buffer end and replace.
                    let body = self.input_buffer.trim_end();
                    // Find last space to strip partial
                    let base = body.rfind(' ').map(|i| &body[..=i]).unwrap_or(&body);
                    base.to_string()
                };
                self.input_buffer = format!("{}{} ", prefix, item.value);
                self.input_cursor = self.input_buffer.len();
            }
            _ => {}
        }
        // Update completions for the new buffer state.
        self.update_completion_overlay();
    }

    // ── Chat display ──────────────────────────────────────────────────────────

    /// Rebuild `chat_lines` and `segment_line_ranges` from `chat_segments` and
    /// the streaming buffer.
    fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges    = Vec::new();
        let mut line_start = 0usize;
        let ascii = self.ascii();
        let bar_char = if ascii { "| " } else { "▌ " };

        // The bar prefix is 2 display columns wide ("| " or "▌ ").
        // Subtract that so the markdown renderer fills exactly the inner width
        // and Ratatui's Paragraph does not need a second wrap pass.
        let bar_cols: u16 = 2;
        let effective_width = self.last_chat_inner_width.saturating_sub(bar_cols).max(20);
        let render_width = if self.config.tui.wrap_width == 0 {
            effective_width
        } else {
            self.config.tui.wrap_width.min(effective_width)
        };

        for (i, seg) in self.chat_segments.iter().enumerate() {
            let s = if self.no_nvim && self.collapsed_segments.contains(&i) {
                collapsed_preview(seg, &self.tool_args_cache)
            } else {
                segment_to_markdown(seg, &self.tool_args_cache)
            };
            let lines = render_markdown(&s, render_width, ascii);
            let (bar_style, dim) = segment_bar_style(seg);
            let styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);
            let n = styled.len();
            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;
        }
        if !self.streaming_assistant_buffer.is_empty() {
            let (s, bar_color) = if self.streaming_is_thinking {
                let prefix = if self.chat_segments.is_empty() { "💭 **Thinking…**\n" } else { "\n💭 **Thinking…**\n" };
                (
                    format!("{}{}", prefix, self.streaming_assistant_buffer),
                    Some(Style::default().fg(Color::Rgb(160, 100, 200))),
                )
            } else {
                let prefix = if self.chat_segments.is_empty() { "**Agent:** " } else { "\n**Agent:** " };
                (
                    format!("{}{}", prefix, self.streaming_assistant_buffer),
                    Some(Style::default().fg(Color::Blue)),
                )
            };
            let lines = render_markdown(&s, render_width, ascii);
            let styled = apply_bar_and_dim(lines, bar_color, false, bar_char);
            all_lines.extend(styled);
        }
        self.chat_lines = all_lines;
        self.segment_line_ranges = ranges;
    }

    async fn rerender_chat(&mut self) {
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let content = format_conversation(
                &self.chat_segments,
                &self.streaming_assistant_buffer,
                &self.tool_args_cache,
            );
            let mut bridge = nvim_bridge.lock().await;
            if let Err(e) = bridge.set_modifiable(true).await {
                tracing::error!("Failed to set buffer modifiable for update: {}", e);
            }
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
            if self.agent_busy {
                if let Err(e) = bridge.set_modifiable(false).await {
                    tracing::error!("Failed to set buffer non-modifiable: {}", e);
                }
            }
        }
        self.build_display_from_segments();
        self.search.update_matches(&self.chat_lines);
    }

    fn ascii(&self) -> bool {
        if std::env::var("SVEN_ASCII_BORDERS").as_deref() == Ok("1") {
            return true;
        }
        self.config.tui.ascii_borders
    }

    fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.auto_scroll = false;
    }

    fn scroll_down(&mut self, n: u16) {
        let max = (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        self.scroll_offset = (self.scroll_offset + n).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Persist the conversation to disk asynchronously.
    fn save_history_async(&mut self) {
        // Collect all segment types as ConversationRecord for full-fidelity JSONL.
        let records: Vec<sven_input::ConversationRecord> = self
            .chat_segments
            .iter()
            .filter_map(|seg| match seg {
                ChatSegment::Message(m) => {
                    Some(sven_input::ConversationRecord::Message(m.clone()))
                }
                ChatSegment::Thinking { content } => {
                    Some(sven_input::ConversationRecord::Thinking { content: content.clone() })
                }
                ChatSegment::ContextCompacted { tokens_before, tokens_after } => {
                    Some(sven_input::ConversationRecord::ContextCompacted {
                        tokens_before: *tokens_before,
                        tokens_after: *tokens_after,
                    })
                }
                ChatSegment::Error(_) => None, // transient; not worth persisting
            })
            .collect();

        if records.is_empty() {
            return;
        }

        // Derive plain messages for the markdown history (existing code path).
        let messages: Vec<sven_model::Message> = records
            .iter()
            .filter_map(|r| {
                if let sven_input::ConversationRecord::Message(m) = r {
                    Some(m.clone())
                } else {
                    None
                }
            })
            .collect();

        // Write full-fidelity JSONL file (complete overwrite) when --jsonl is set.
        if let Some(jsonl_path) = self.jsonl_path.clone() {
            let serialized = sven_input::serialize_jsonl_records(&records);
            tokio::spawn(async move {
                if let Err(e) = std::fs::write(&jsonl_path, &serialized) {
                    tracing::debug!("failed to update JSONL conversation file: {e}");
                }
            });
        }

        if messages.is_empty() {
            return;
        }

        let path_opt = self.history_path.clone();
        match path_opt {
            None => {
                match sven_input::history::save(&messages) {
                    Ok(path) => {
                        debug!(path = %path.display(), "conversation saved to history");
                        self.history_path = Some(path);
                    }
                    Err(e) => debug!("failed to save conversation to history: {e}"),
                }
            }
            Some(path) => {
                tokio::spawn(async move {
                    if let Err(e) = sven_input::history::save_to(&path, &messages) {
                        debug!("failed to update conversation history: {e}");
                    }
                });
            }
        }
    }

    fn scroll_to_bottom(&mut self) {
        if self.nvim_bridge.is_none() && self.auto_scroll {
            self.scroll_offset =
                (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        }
    }

    /// Adjust `input_scroll_offset` so the cursor row is within the visible
    /// window of the input pane.  Must be called after any change to
    /// `input_buffer` or `input_cursor`.
    fn adjust_input_scroll(&mut self) {
        let w = self.last_input_inner_width as usize;
        let h = self.last_input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(
            &self.input_buffer,
            w,
            self.input_cursor,
        );
        crate::input_wrap::adjust_scroll(
            wrap.cursor_row,
            h,
            &mut self.input_scroll_offset,
        );
    }

    /// Adjust `edit_scroll_offset` so the cursor row is within the visible
    /// window of the input pane when in inline edit mode.
    fn adjust_edit_scroll(&mut self) {
        let w = self.last_input_inner_width as usize;
        let h = self.last_input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(
            &self.edit_buffer,
            w,
            self.edit_cursor,
        );
        crate::input_wrap::adjust_scroll(
            wrap.cursor_row,
            h,
            &mut self.edit_scroll_offset,
        );
    }

    /// Read the Neovim buffer and update `chat_segments` from its current
    /// content.  Called before submitting so in-buffer edits are preserved.
    async fn sync_nvim_buffer_to_segments(&mut self) {
        let content = if let Some(nvim_bridge) = &self.nvim_bridge {
            let bridge = nvim_bridge.lock().await;
            bridge.get_buffer_content().await.ok()
        } else {
            return;
        };
        if let Some(content) = content {
            match parse_markdown_to_messages(&content) {
                Ok(messages) if !messages.is_empty() => {
                    self.chat_segments = messages
                        .iter()
                        .map(|m| ChatSegment::Message(m.clone()))
                        .collect();
                    self.tool_args_cache.clear();
                    for m in &messages {
                        if let MessageContent::ToolCall { tool_call_id, function } = &m.content {
                            self.tool_args_cache
                                .insert(tool_call_id.clone(), function.name.clone());
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("sync_nvim_buffer_to_segments: parse error — keeping existing segments: {e}");
                }
            }
        }
    }

    async fn nvim_scroll_to_bottom(&self) {
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let mut bridge = nvim_bridge.lock().await;
            let _ = bridge.send_input("G").await;
        }
    }

    /// Update the segment being edited with the current `edit_buffer` content
    /// (live preview while the user types).
    fn update_editing_segment_live(&mut self) {
        if let Some(idx) = self.editing_message_index {
            let new_text = self.edit_buffer.clone();
            match self.chat_segments.get_mut(idx) {
                Some(ChatSegment::Message(m)) => {
                    match (&m.role, &mut m.content) {
                        (Role::User, MessageContent::Text(t)) => { *t = new_text; }
                        (Role::Assistant, MessageContent::Text(t)) => { *t = new_text; }
                        _ => {}
                    }
                }
                _ => {}
            }
            self.build_display_from_segments();
            self.search.update_matches(&self.chat_lines);
        }
    }

    /// Apply an `Input*` action to the current `(edit_buffer, edit_cursor)`.
    ///
    /// Returns `Some((new_buf, new_cur))` when the action was consumed by the
    /// edit mode; returns `None` for non-input actions.
    fn apply_input_to_edit(&self, action: &Action) -> Option<(String, usize)> {
        let (buf, cur) = (&self.edit_buffer, self.edit_cursor);
        let mut buf = buf.clone();
        let mut cur = cur;
        match action {
            Action::InputChar(c) => {
                buf.insert(cur, *c);
                cur += c.len_utf8();
            }
            Action::InputNewline => {
                buf.insert(cur, '\n');
                cur += 1;
            }
            Action::InputBackspace => {
                if cur > 0 {
                    let prev = prev_char_boundary(&buf, cur);
                    buf.remove(prev);
                    cur = prev;
                }
            }
            Action::InputDelete => {
                if cur < buf.len() {
                    buf.remove(cur);
                }
            }
            Action::InputMoveCursorLeft  => cur = prev_char_boundary(&buf, cur),
            Action::InputMoveCursorRight => {
                if cur < buf.len() {
                    let ch = buf[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    cur += ch;
                }
            }
            Action::InputMoveWordLeft  => cur = prev_word_boundary(&buf, cur),
            Action::InputMoveWordRight => cur = next_word_boundary(&buf, cur),
            Action::InputMoveLineStart => cur = 0,
            Action::InputMoveLineEnd   => cur = buf.len(),
            Action::InputMoveLineUp => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row > 0 {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf, w, ws.cursor_row - 1, ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf, w, ws.cursor_row + 1, ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputDeleteToEnd   => buf.truncate(cur),
            Action::InputDeleteToStart => {
                buf = buf[cur..].to_string();
                cur = 0;
            }
            _ => return None,
        }
        Some((buf, cur))
    }
}

// ── Character and word boundary helpers ──────────────────────────────────────

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 { return 0; }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) { p -= 1; }
    p
}

fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes   = &s.as_bytes()[..pos];
    let trimmed = bytes.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    bytes[..trimmed].iter().rposition(|&b| b == b' ').map(|i| i + 1).unwrap_or(0)
}

fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[pos..];
    let start = bytes.iter().position(|&b| b != b' ').unwrap_or(0);
    let end   = bytes[start..].iter().position(|&b| b == b' ').unwrap_or(bytes.len() - start);
    pos + start + end
}
