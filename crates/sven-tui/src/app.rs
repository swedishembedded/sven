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
use sven_config::{AgentMode, Config};
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
        segment::{messages_for_resubmit, segment_at_line, segment_editable_text, ChatSegment},
    },
    input::{is_reserved_key, to_nvim_notation},
    keys::{map_key, Action},
    layout::AppLayout,
    markdown::{render_markdown, StyledLines},
    nvim::NvimBridge,
    overlay::question::QuestionModal,
    pager::PagerOverlay,
    widgets::{draw_chat, draw_help, draw_input, draw_question_modal, draw_search, draw_status},
};

// ── Public types ──────────────────────────────────────────────────────────────

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
}

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
}

// ── App ───────────────────────────────────────────────────────────────────────

/// The top-level TUI application state.
pub struct App {
    config: Arc<Config>,
    mode: AgentMode,
    focus: FocusPane,
    chat_lines: StyledLines,
    /// Structured segments (messages + context-compacted notes).
    /// Source of truth for display and resubmit.
    chat_segments: Vec<ChatSegment>,
    /// Accumulated assistant text during streaming until `TextComplete`.
    streaming_assistant_buffer: String,
    /// For each segment index: `(start_line, end_line)` in `chat_lines`.
    /// Rebuilt whenever `build_display_from_segments` runs.
    segment_line_ranges: Vec<(usize, usize)>,
    scroll_offset: u16,
    input_buffer: String,
    input_cursor: usize,
    queued: VecDeque<String>,
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
    /// When set, we are in inline edit mode.
    editing_message_index: Option<usize>,
    edit_buffer: String,
    edit_cursor: usize,
    /// Original text saved for cancel/restore.
    edit_original_text: Option<String>,
    nvim_bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
    nvim_flush_notify:  Option<Arc<tokio::sync::Notify>>,
    nvim_submit_notify: Option<Arc<tokio::sync::Notify>>,
    nvim_quit_notify:   Option<Arc<tokio::sync::Notify>>,
    history_path: Option<PathBuf>,
    no_nvim: bool,
}

impl App {
    pub fn new(config: Arc<Config>, opts: AppOptions) -> Self {
        let (initial_segments, history_path) = opts
            .initial_history
            .map(|(segs, path)| (segs, Some(path)))
            .unwrap_or_else(|| (Vec::new(), None));

        let mut app = Self {
            config,
            mode: opts.mode,
            focus: FocusPane::Input,
            chat_lines: Vec::new(),
            chat_segments: initial_segments,
            streaming_assistant_buffer: String::new(),
            segment_line_ranges: Vec::new(),
            scroll_offset: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            queued: VecDeque::new(),
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
            edit_buffer: String::new(),
            edit_cursor: 0,
            edit_original_text: None,
            nvim_bridge: None,
            nvim_flush_notify: None,
            nvim_submit_notify: None,
            nvim_quit_notify: None,
            history_path,
            no_nvim: opts.no_nvim,
        };
        if let Some(prompt) = opts.initial_prompt {
            app.queued.push_back(prompt);
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

        let cfg  = self.config.clone();
        let mode = self.mode;
        tokio::spawn(async move {
            agent_task(cfg, mode, submit_rx, event_tx, question_tx).await;
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

        if let Some(p) = self.queued.pop_front() {
            self.chat_segments.push(ChatSegment::Message(Message::user(&p)));
            self.rerender_chat().await;
            self.send_to_agent(p).await;
        }

        let mut crossterm_events = EventStream::new();

        loop {
            if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.search.active,
                );
                self.chat_height = layout.chat_inner_height().max(1);
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

                let layout = AppLayout::new(frame, self.search.active);

                draw_status(
                    frame, layout.status_bar, &self.config.model.name,
                    self.mode, self.context_pct, self.cache_hit_pct, self.agent_busy,
                    self.current_tool.as_deref(), ascii,
                );

                let lines_to_draw = if !nvim_lines.is_empty() { &nvim_lines } else { &self.chat_lines };
                draw_chat(
                    frame, layout.chat_pane, lines_to_draw, nvim_draw_scroll,
                    self.focus == FocusPane::Chat, ascii,
                    &self.search.query, &self.search.matches, self.search.current,
                    self.search.regex.as_ref(), nvim_cursor,
                );
                draw_input(
                    frame, layout.input_pane,
                    if self.editing_message_index.is_some() { &self.edit_buffer } else { &self.input_buffer },
                    if self.editing_message_index.is_some() { self.edit_cursor } else { self.input_cursor },
                    self.focus == FocusPane::Input || self.editing_message_index.is_some(),
                    self.queued.len(), ascii, self.editing_message_index.is_some(),
                );
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
                if let Some(next) = self.queued.pop_front() {
                    let tx = self.agent_tx.clone().unwrap();
                    tokio::spawn(async move { let _ = tx.send(AgentRequest::Submit(next)).await; });
                    self.agent_busy = true;
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
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
            }
            AgentEvent::ThinkingComplete(content) => {
                self.streaming_assistant_buffer.clear();
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

                let in_edit_mode = self.editing_message_index.is_some();
                if let Some(action) = map_key(k, in_search, in_input, self.pending_nav, in_edit_mode) {
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
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    let _ = bridge.send_input("<C-y><C-y><C-y>").await;
                                }
                            } else {
                                self.scroll_up(3);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if self.nvim_bridge.is_some() {
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
                            let content_start_row: u16 = 2;
                            if mouse.row >= content_start_row {
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
        if self.editing_message_index.is_some() {
            if let Some((buf, cur)) = self.apply_input_to_edit(&action) {
                self.edit_buffer = buf;
                self.edit_cursor = cur;
                self.update_editing_segment_live();
                self.rerender_chat().await;
                return false;
            }
        }

        match action {
            Action::FocusChat  => self.focus = FocusPane::Chat,
            Action::FocusInput => self.focus = FocusPane::Input,

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
            Action::EditMessageConfirm => {
                if let Some(i) = self.editing_message_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_message_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
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
                            self.send_resubmit_to_agent(messages, new_content).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(i) {
                                m.content = MessageContent::Text(new_content);
                            }
                            self.build_display_from_segments();
                            self.search.update_matches(&self.chat_lines);
                            self.rerender_chat().await;
                        }
                        _ => {}
                    }
                }
            }
            Action::EditMessageCancel => {
                if let Some(idx) = self.editing_message_index {
                    if let Some(original) = &self.edit_original_text {
                        if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(idx) {
                            match (&m.role, &mut m.content) {
                                (Role::User, MessageContent::Text(t)) => { *t = original.clone(); }
                                (Role::Assistant, MessageContent::Text(t)) => { *t = original.clone(); }
                                _ => {}
                            }
                        }
                        self.build_display_from_segments();
                        self.search.update_matches(&self.chat_lines);
                    }
                }
                self.editing_message_index = None;
                self.edit_buffer.clear();
                self.edit_cursor = 0;
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
                            self.send_resubmit_to_agent(messages, new_user_content).await;
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
                }
            }
            Action::ScrollBottom => {
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
            Action::InputDeleteToEnd   => self.input_buffer.truncate(self.input_cursor),
            Action::InputDeleteToStart => {
                self.input_buffer = self.input_buffer[self.input_cursor..].to_string();
                self.input_cursor = 0;
            }

            Action::Submit => {
                let text = std::mem::take(&mut self.input_buffer).trim().to_string();
                self.input_cursor = 0;
                if text.eq_ignore_ascii_case("/quit") {
                    return true;
                }
                if !text.is_empty() {
                    if self.agent_busy {
                        self.queued.push_back(text);
                    } else {
                        self.sync_nvim_buffer_to_segments().await;
                        let history = messages_for_resubmit(&self.chat_segments);
                        self.chat_segments.push(ChatSegment::Message(Message::user(&text)));
                        self.rerender_chat().await;
                        self.scroll_to_bottom();
                        self.send_resubmit_to_agent(history, text).await;
                    }
                }
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

    async fn send_to_agent(&mut self, text: String) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx.send(AgentRequest::Submit(text)).await;
            self.agent_busy = true;
        }
    }

    async fn send_resubmit_to_agent(&mut self, messages: Vec<Message>, new_user_content: String) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Resubmit { messages, new_user_content })
                .await;
            self.agent_busy = true;
        }
    }

    // ── Chat display ──────────────────────────────────────────────────────────

    /// Rebuild `chat_lines` and `segment_line_ranges` from `chat_segments` and
    /// the streaming buffer.
    fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges    = Vec::new();
        let mut line_start = 0usize;
        let bar_char = if self.ascii() { "| " } else { "▌ " };

        for (i, seg) in self.chat_segments.iter().enumerate() {
            let s = if self.no_nvim && self.collapsed_segments.contains(&i) {
                collapsed_preview(seg, &self.tool_args_cache)
            } else {
                segment_to_markdown(seg, &self.tool_args_cache)
            };
            let lines = render_markdown(&s, self.config.tui.wrap_width, self.ascii());
            let (bar_style, dim) = segment_bar_style(seg);
            let styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);
            let n = styled.len();
            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;
        }
        if !self.streaming_assistant_buffer.is_empty() {
            let prefix = if self.chat_segments.is_empty() { "**Agent:** " } else { "\n**Agent:** " };
            let s = format!("{}{}", prefix, self.streaming_assistant_buffer);
            let lines = render_markdown(&s, self.config.tui.wrap_width, self.ascii());
            let styled = apply_bar_and_dim(
                lines,
                Some(Style::default().fg(Color::Blue)),
                false,
                bar_char,
            );
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
    }

    fn scroll_down(&mut self, n: u16) {
        let max = (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    /// Persist the conversation to disk asynchronously.
    fn save_history_async(&mut self) {
        let messages: Vec<sven_model::Message> = self
            .chat_segments
            .iter()
            .filter_map(|seg| {
                if let ChatSegment::Message(m) = seg { Some(m.clone()) } else { None }
            })
            .collect();
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
        if self.nvim_bridge.is_none() {
            self.scroll_offset =
                (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        }
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
            if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(idx) {
                match (&m.role, &mut m.content) {
                    (Role::User, MessageContent::Text(t)) => { *t = self.edit_buffer.clone(); }
                    (Role::Assistant, MessageContent::Text(t)) => { *t = self.edit_buffer.clone(); }
                    _ => {}
                }
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
