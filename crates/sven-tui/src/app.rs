use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::{AskQuestionTool, FsTool, GlobTool, QuestionRequest, ShellTool, TodoItem, ToolRegistry};

use crate::{
    keys::{map_key, Action},
    layout::AppLayout,
    markdown::{render_markdown, StyledLines},
    nvim_bridge::NvimBridge,
    pager::PagerOverlay,
    widgets::{
        draw_chat, draw_help, draw_input, draw_question_modal, draw_search, draw_status,
    },
};

/// Options passed when constructing the TUI app.
pub struct AppOptions {
    pub mode: AgentMode,
    pub initial_prompt: Option<String>,
}

/// Which pane currently holds keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Chat,
    Input,
}

/// One segment in the chat display (message or display-only note).
#[derive(Debug, Clone)]
pub enum ChatSegment {
    Message(Message),
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
    },
    Error(String),
}

/// Request from TUI to the background agent task.
#[derive(Debug)]
pub enum AgentRequest {
    /// Submit a new user message (normal flow).
    Submit(String),
    /// Replace history and submit (edit-and-resubmit flow).
    Resubmit {
        messages: Vec<Message>,
        new_user_content: String,
    },
}

// ── Key filtering for Neovim forwarding ──────────────────────────────────────

/// Check if a key event is reserved for sven (not forwarded to Neovim)
fn is_reserved_key(event: &KeyEvent) -> bool {
    matches!(
        (event.modifiers, event.code),
        (KeyModifiers::CONTROL, KeyCode::Char('w'))  // Pane switching prefix
        | (KeyModifiers::CONTROL, KeyCode::Char('c'))  // Quit
        | (KeyModifiers::CONTROL, KeyCode::Char('t'))  // Pager toggle
        | (KeyModifiers::NONE, KeyCode::F(1))  // Help
        | (KeyModifiers::NONE, KeyCode::F(4))  // Mode cycle
        | (KeyModifiers::NONE, KeyCode::Char('/'))  // Search (when not in nvim)
    )
}

/// Convert a crossterm KeyEvent to Neovim key notation
fn to_nvim_notation(event: &KeyEvent) -> Option<String> {
    let key_str = match event.code {
        KeyCode::Char(c) => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                format!("<C-{}>", c)
            } else if event.modifiers.contains(KeyModifiers::ALT) {
                format!("<A-{}>", c)
            } else if event.modifiers.contains(KeyModifiers::SHIFT) && c.is_alphabetic() {
                c.to_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => {
            if event.modifiers.contains(KeyModifiers::SHIFT) {
                "<S-CR>".to_string()
            } else {
                "<CR>".to_string()
            }
        }
        KeyCode::Esc => "<Esc>".to_string(),
        KeyCode::Backspace => "<BS>".to_string(),
        KeyCode::Delete => "<Del>".to_string(),
        KeyCode::Tab => {
            if event.modifiers.contains(KeyModifiers::SHIFT) {
                "<S-Tab>".to_string()
            } else {
                "<Tab>".to_string()
            }
        }
        KeyCode::Up => "<Up>".to_string(),
        KeyCode::Down => "<Down>".to_string(),
        KeyCode::Left => "<Left>".to_string(),
        KeyCode::Right => "<Right>".to_string(),
        KeyCode::Home => "<Home>".to_string(),
        KeyCode::End => "<End>".to_string(),
        KeyCode::PageUp => "<PageUp>".to_string(),
        KeyCode::PageDown => "<PageDown>".to_string(),
        KeyCode::F(n) => format!("<F{}>", n),
        _ => return None,
    };
    
    Some(key_str)
}

// ── Search state ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct SearchState {
    active: bool,
    query: String,
    matches: Vec<usize>,
    current: usize,
    /// Compiled regex (when the query is valid regex syntax).
    regex: Option<regex::Regex>,
}

impl SearchState {
    fn update_matches(&mut self, lines: &StyledLines) {
        if self.query.is_empty() {
            self.matches.clear();
            self.regex = None;
            return;
        }

        // Try to compile the query as a case-insensitive regex
        let re = regex::Regex::new(&format!("(?i){}", &self.query)).ok();
        self.regex = re.clone();

        self.matches = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                if let Some(re) = &re {
                    l.spans.iter().any(|s| re.is_match(&s.content))
                } else {
                    // Invalid regex — fall back to literal case-insensitive
                    let q = self.query.to_lowercase();
                    l.spans.iter().any(|s| s.content.to_lowercase().contains(&q))
                }
            })
            .map(|(i, _)| i)
            .collect();

        if self.current >= self.matches.len() {
            self.current = 0;
        }
    }

    fn current_line(&self) -> Option<usize> {
        self.matches.get(self.current).copied()
    }
}

// ── Question modal ────────────────────────────────────────────────────────────

struct QuestionModal {
    questions: Vec<String>,
    /// Answers collected so far (one per completed question).
    answers: Vec<String>,
    current_q: usize,
    input: String,
    cursor: usize,
    answer_tx: oneshot::Sender<String>,
}

impl QuestionModal {
    fn new(questions: Vec<String>, answer_tx: oneshot::Sender<String>) -> Self {
        Self {
            questions,
            answers: Vec::new(),
            current_q: 0,
            input: String::new(),
            cursor: 0,
            answer_tx,
        }
    }

    /// Submit the current input as the answer to the current question.
    /// Returns `true` if all questions are answered (modal should close).
    fn submit(&mut self) -> bool {
        let answer = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.answers.push(format!(
            "Q: {}\nA: {}",
            self.questions[self.current_q],
            answer,
        ));
        self.current_q += 1;
        self.current_q >= self.questions.len()
    }

    /// Build the final answer string and consume the modal, sending the answer.
    fn finish(self) {
        let combined = self.answers.join("\n\n");
        let _ = self.answer_tx.send(combined);
    }

    /// Cancel the modal, sending a cancellation notice.
    fn cancel(self) {
        let _ = self.answer_tx.send(
            "The user cancelled the question. Proceed with your best judgement.".into(),
        );
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

/// The top-level TUI application state.
pub struct App {
    config: Arc<Config>,
    mode: AgentMode,
    focus: FocusPane,
    chat_lines: StyledLines,
    /// Display string built from chat_segments + streaming buffer; re-rendered on resize or new content.
    chat_raw: String,
    /// Structured segments (messages + context compacted notes). Source of truth for display and resubmit.
    chat_segments: Vec<ChatSegment>,
    /// Accumulated assistant text during streaming until TextComplete.
    streaming_assistant_buffer: String,
    /// For each segment index, (start_line, end_line) in chat_lines. Built when rerendering.
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
    /// Track if we need to add "Agent:" prefix for next text delta
    needs_agent_prefix: bool,
    agent_tx: Option<mpsc::Sender<AgentRequest>>,
    event_rx: Option<mpsc::Receiver<AgentEvent>>,
    pending_nav: bool,
    chat_height: u16,
    /// Full-screen pager overlay (Ctrl+T).
    pager: Option<PagerOverlay>,
    /// Active ask-question modal.
    question_modal: Option<QuestionModal>,
    /// Args preview cache: call_id → formatted args string.
    tool_args_cache: HashMap<String, String>,
    /// When set, we're in edit mode: edit_buffer/edit_cursor are active.
    editing_message_index: Option<usize>,
    edit_buffer: String,
    edit_cursor: usize,
    /// Embedded Neovim instance for chat view
    nvim_bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
}

impl App {
    pub fn new(config: Arc<Config>, opts: AppOptions) -> Self {
        let mut app = Self {
            config,
            mode: opts.mode,
            focus: FocusPane::Input,
            chat_lines: Vec::new(),
            chat_raw: String::new(),
            chat_segments: Vec::new(),
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
            needs_agent_prefix: false,
            agent_tx: None,
            event_rx: None,
            pending_nav: false,
            chat_height: 24,
            pager: None,
            question_modal: None,
            tool_args_cache: HashMap::new(),
            editing_message_index: None,
            edit_buffer: String::new(),
            edit_cursor: 0,
            nvim_bridge: None,
        };
        if let Some(prompt) = opts.initial_prompt {
            app.queued.push_back(prompt);
        }
        app
    }

    /// Run the TUI event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(4);

        self.agent_tx = Some(submit_tx);
        self.event_rx = Some(event_rx);

        let cfg = self.config.clone();
        let mode = self.mode;

        tokio::spawn(async move {
            agent_task(cfg, mode, submit_rx, event_tx, question_tx).await;
        });

        // Initialize NvimBridge with proper dimensions
        let (nvim_width, nvim_height) = if let Ok(size) = terminal.size() {
            let layout = AppLayout::compute(
                Rect::new(0, 0, size.width, size.height),
                false,  // search not active initially
            );
            (
                layout.chat_pane.width.saturating_sub(2),  // Account for border
                layout.chat_inner_height().max(1),
            )
        } else {
            (80, 24)  // Fallback dimensions
        };

        match NvimBridge::spawn(nvim_width, nvim_height).await {
            Ok(mut bridge) => {
                // Configure the buffer
                if let Err(e) = bridge.configure_buffer().await {
                    tracing::warn!("Failed to configure Neovim buffer: {}", e);
                }
                self.nvim_bridge = Some(Arc::new(tokio::sync::Mutex::new(bridge)));
            }
            Err(e) => {
                tracing::error!("Failed to spawn Neovim: {}. Chat view will be degraded.", e);
                // Continue without Neovim - we'll handle this gracefully
            }
        }

        if let Some(p) = self.queued.pop_front() {
            self.chat_segments
                .push(ChatSegment::Message(Message::user(&p)));
            self.rerender_chat().await;
            self.send_to_agent(p).await;
        }

        let mut crossterm_events = EventStream::new();

        loop {
            // Pre-compute layout so scroll helpers have correct heights.
            if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.search.active,
                );
                self.chat_height = layout.chat_inner_height().max(1);
            }

            let ascii = self.ascii();

            // Render Neovim grid to lines before drawing (if available)
            let nvim_lines = if let Some(nvim_bridge) = &self.nvim_bridge {
                let bridge = nvim_bridge.lock().await;
                bridge.render_to_lines(self.scroll_offset, self.chat_height).await
            } else {
                Vec::new()
            };

            let nvim_cursor = if let Some(nvim_bridge) = &self.nvim_bridge {
                let bridge = nvim_bridge.lock().await;
                Some(bridge.get_cursor_pos().await)
            } else {
                None
            };

            terminal.draw(|frame| {
                // Pager overlay takes the whole screen
                if let Some(pager) = &mut self.pager {
                    pager.render(
                        frame,
                        &self.search.matches,
                        self.search.current,
                        &self.search.query,
                        self.search.regex.as_ref(),
                        ascii,
                    );
                    // Draw search bar on top of pager if active
                    if self.search.active {
                        let area = frame.area();
                        let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                        draw_search(
                            frame,
                            search_area,
                            &self.search.query,
                            self.search.matches.len(),
                            self.search.current,
                        );
                    }
                    return;
                }

                // Normal layout
                let layout = AppLayout::new(frame, self.search.active);

                draw_status(
                    frame,
                    layout.status_bar,
                    &self.config.model.name,
                    self.mode,
                    self.context_pct,
                    self.agent_busy,
                    self.current_tool.as_deref(),
                    ascii,
                );
                // Use Neovim lines if available, otherwise fall back to chat_lines
                let lines_to_draw = if !nvim_lines.is_empty() {
                    &nvim_lines
                } else {
                    &self.chat_lines
                };
                
                draw_chat(
                    frame,
                    layout.chat_pane,
                    lines_to_draw,
                    self.scroll_offset,
                    self.focus == FocusPane::Chat,
                    ascii,
                    &self.search.query,
                    &self.search.matches,
                    self.search.current,
                    self.search.regex.as_ref(),
                    nvim_cursor,
                );
                draw_input(
                    frame,
                    layout.input_pane,
                    if self.editing_message_index.is_some() {
                        &self.edit_buffer
                    } else {
                        &self.input_buffer
                    },
                    if self.editing_message_index.is_some() {
                        self.edit_cursor
                    } else {
                        self.input_cursor
                    },
                    self.focus == FocusPane::Input || self.editing_message_index.is_some(),
                    self.queued.len(),
                    ascii,
                    self.editing_message_index.is_some(),
                );
                if self.search.active {
                    draw_search(
                        frame,
                        layout.search_bar,
                        &self.search.query,
                        self.search.matches.len(),
                        self.search.current,
                    );
                }
                if self.show_help {
                    draw_help(frame, ascii);
                }

                // Question modal rendered on top of everything
                if let Some(modal) = &self.question_modal {
                    draw_question_modal(
                        frame,
                        &modal.questions,
                        modal.current_q,
                        &modal.input,
                        modal.cursor,
                        ascii,
                    );
                }
            })?;

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
            }
        }

        Ok(())
    }

    async fn recv_agent_event(&mut self) -> Option<AgentEvent> {
        if let Some(rx) = &mut self.event_rx { rx.recv().await } else { None }
    }

    // ── Agent event handler ───────────────────────────────────────────────────

    async fn handle_agent_event(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::TextDelta(delta) => {
                if self.needs_agent_prefix {
                    self.needs_agent_prefix = false;
                }
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::TextComplete(full_text) => {
                self.chat_segments
                    .push(ChatSegment::Message(Message::assistant(&full_text)));
                self.streaming_assistant_buffer.clear();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                // Set buffer back to modifiable after streaming completes
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
            }
            AgentEvent::ToolCallStarted(tc) => {
                // Store tool name with call ID for later lookup
                self.tool_args_cache.insert(tc.id.clone(), tc.name.clone());
                self.current_tool = Some(tc.name.clone());
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
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallFinished { call_id, output, .. } => {
                self.current_tool = None;
                self.chat_segments
                    .push(ChatSegment::Message(Message::tool_result(&call_id, &output)));
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
                self.chat_segments.push(ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                });
                self.rerender_chat().await;
            }
            AgentEvent::TokenUsage { input, output, .. } => {
                let max = 128_000u32;
                self.context_pct = ((input + output) * 100 / max.max(1)).min(100) as u8;
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                self.current_tool = None;
                // Set buffer to modifiable when turn is complete
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
                if let Some(next) = self.queued.pop_front() {
                    let tx = self.agent_tx.clone().unwrap();
                    tokio::spawn(async move { let _ = tx.send(AgentRequest::Submit(next)).await; });
                    self.agent_busy = true;
                    self.needs_agent_prefix = true;
                }
            }
            AgentEvent::Error(msg) => {
                self.chat_segments.push(ChatSegment::Error(msg.clone()));
                self.rerender_chat().await;
                self.agent_busy = false;
                self.current_tool = None;
            }
            AgentEvent::TodoUpdate(todos) => {
                // Format todos as markdown and add to conversation
                let todo_md = format_todos_markdown(&todos);
                self.chat_segments.push(ChatSegment::Message(Message::assistant(&todo_md)));
                self.rerender_chat().await;
                
                // Trigger Neovim to refresh todo enhancements
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.refresh_todo_display().await {
                        tracing::warn!("Failed to refresh todo display: {}", e);
                    }
                }
            }
            _ => {}
        }
        false
    }

    // ── Question request handler ──────────────────────────────────────────────

    fn handle_question_request(&mut self, req: QuestionRequest) {
        debug!(id = %req.id, count = req.questions.len(), "question request received");
        self.question_modal = Some(QuestionModal::new(req.questions, req.answer_tx));
        // Focus the input pane to make typing natural
        self.focus = FocusPane::Input;
    }

    // ── Terminal event handler ────────────────────────────────────────────────

    async fn handle_term_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                // Help overlay: dismiss on any key
                if self.show_help {
                    self.show_help = false;
                    return false;
                }

                // ── Question modal takes priority ─────────────────────────────
                if self.question_modal.is_some() {
                    return self.handle_modal_key(k);
                }

                // ── Pager overlay ─────────────────────────────────────────────
                if self.pager.is_some() {
                    return self.handle_pager_key(k).await;
                }

                // ── Normal mode ───────────────────────────────────────────────
                let in_search = self.search.active;
                let in_input  = self.focus == FocusPane::Input;

                // ── Neovim key forwarding ──────────────────────────────────────
                // If chat is focused, nvim_bridge exists, key is not reserved,
                // AND not in pending_nav state, forward to Neovim
                if self.focus == FocusPane::Chat 
                    && !in_search 
                    && !self.pending_nav  // Don't forward if waiting for nav chord completion
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
                // Only scroll when pager is not open (pager uses keyboard)
                if self.pager.is_none() {
                    match mouse.kind {
                        MouseEventKind::ScrollUp   => self.scroll_up(3),
                        MouseEventKind::ScrollDown => self.scroll_down(3),
                        _ => {}
                    }
                }
                false
            }

            Event::Resize(width, height) => {
                // Update Neovim UI dimensions
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    // Calculate chat pane dimensions
                    let layout = AppLayout::compute(
                        Rect::new(0, 0, width, height),
                        self.search.active,
                    );
                    let chat_width = layout.chat_pane.width.saturating_sub(2);  // Account for border
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
            KeyCode::Backspace => {
                if modal.cursor > 0 {
                    let prev = prev_char_boundary(&modal.input, modal.cursor);
                    modal.input.remove(prev);
                    modal.cursor = prev;
                }
            }
            KeyCode::Left => {
                modal.cursor = prev_char_boundary(&modal.input, modal.cursor);
            }
            KeyCode::Right => {
                if modal.cursor < modal.input.len() {
                    let ch = modal.input[modal.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    modal.cursor += ch;
                }
            }
            KeyCode::Home => { modal.cursor = 0; }
            KeyCode::End  => { modal.cursor = modal.input.len(); }
            KeyCode::Char(c) => {
                modal.input.insert(modal.cursor, c);
                modal.cursor += c.len_utf8();
            }
            _ => {}
        }
        false
    }

    // ── Pager key handling ────────────────────────────────────────────────────

    async fn handle_pager_key(&mut self, k: crossterm::event::KeyEvent) -> bool {
        use crate::keys::map_search_key;
        use crate::pager::PagerAction;

        // Let search bar handle keys when active
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
        // When in edit mode, route Input* actions to edit buffer
        if self.editing_message_index.is_some() {
            if let Some((buf, cur)) = self.apply_input_to_edit(&action) {
                self.edit_buffer = buf;
                self.edit_cursor = cur;
                return false;
            }
        }

        match action {
            Action::Quit => return true,

            Action::FocusChat  => self.focus = FocusPane::Chat,
            Action::FocusInput => self.focus = FocusPane::Input,

            Action::EditMessageAtCursor => {
                let line = self.scroll_offset as usize;
                if let Some(seg_idx) = self.segment_at_line(line) {
                    if let Some(text) = Self::segment_editable_text(&self.chat_segments, seg_idx) {
                        self.editing_message_index = Some(seg_idx);
                        self.edit_cursor = text.len();
                        self.edit_buffer = text;
                    }
                }
            }
            Action::EditMessageConfirm => {
                if let Some(i) = self.editing_message_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_message_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
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
                            let messages = Self::messages_for_resubmit(&self.chat_segments);
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, new_content).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            let last_user_seg = self.chat_segments[..=i]
                                .iter()
                                .rposition(|s| matches!(s, ChatSegment::Message(m) if m.role == Role::User));
                            let keep_end = match last_user_seg {
                                Some(j) => j + 1,
                                None => return false,
                            };
                            self.chat_segments.truncate(keep_end);
                            let messages = Self::messages_for_resubmit(&self.chat_segments);
                            let new_user_content = self.chat_segments
                                .last()
                                .and_then(|s| match s {
                                    ChatSegment::Message(m) => m.as_text().map(String::from),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, new_user_content).await;
                        }
                        _ => {}
                    }
                }
            }
            Action::EditMessageCancel => {
                self.editing_message_index = None;
                self.edit_buffer.clear();
                self.edit_cursor = 0;
            }

            Action::ScrollUp       => self.scroll_up(1),
            Action::ScrollDown     => self.scroll_down(1),
            Action::ScrollPageUp   => self.scroll_up(self.chat_height / 2),
            Action::ScrollPageDown => self.scroll_down(self.chat_height / 2),
            Action::ScrollTop      => self.scroll_offset = 0,
            Action::ScrollBottom   => self.scroll_to_bottom(),

            Action::SearchOpen => {
                self.search.query.clear();
                self.search.current = 0;
                self.search.update_matches(&self.chat_lines);
                self.search.active = true;
                self.focus = FocusPane::Chat;
            }
            Action::SearchClose => {
                self.search.active = false;
                // Scroll pager to current match if pager is open
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
                if !text.is_empty() {
                    if self.agent_busy {
                        self.queued.push_back(text);
                    } else {
                        self.chat_segments
                            .push(ChatSegment::Message(Message::user(&text)));
                        self.rerender_chat().await;
                        self.scroll_to_bottom();
                        self.send_to_agent(text).await;
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
                // If there's a current search match, scroll pager to it
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
            self.needs_agent_prefix = true;
        }
    }

    async fn send_resubmit_to_agent(&mut self, messages: Vec<Message>, new_user_content: String) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Resubmit {
                    messages,
                    new_user_content,
                })
                .await;
            self.agent_busy = true;
            self.needs_agent_prefix = true;
        }
    }

    /// Send the current Neovim buffer content to the agent
    #[allow(dead_code)]
    async fn send_buffer_to_agent(&mut self) {
        let content = if let Some(nvim_bridge) = &self.nvim_bridge {
            let bridge = nvim_bridge.lock().await;
            match bridge.get_buffer_content().await {
                Ok(content) => Some(content),
                Err(e) => {
                    tracing::error!("Failed to get buffer content: {}", e);
                    None
                }
            }
        } else {
            None
        };
        
        if let Some(content) = content {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                self.send_to_agent(trimmed.to_string()).await;
            }
        }
    }

    /// Build chat_lines and segment_line_ranges from chat_segments and streaming buffer.
    /// User messages get a green vertical bar and dimmed text; agent messages get a blue bar.
    fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges = Vec::new();
        let mut line_start = 0usize;
        let bar_char = if self.ascii() { "| " } else { "▌ " };
        for seg in &self.chat_segments {
            let s = segment_to_markdown(seg, &self.tool_args_cache);
            let lines = render_markdown(&s, self.config.tui.wrap_width, self.ascii());
            let (bar_style, dim) = segment_bar_style(seg);
            let styled = apply_bar_and_dim(lines, bar_style, dim, bar_char);
            let n = styled.len();
            all_lines.extend(styled);
            ranges.push((line_start, line_start + n));
            line_start += n;
        }
        if !self.streaming_assistant_buffer.is_empty() {
            let prefix = if self.chat_segments.is_empty() {
                "**Agent:** "
            } else {
                "\n**Agent:** "
            };
            let s = format!("{}{}", prefix, self.streaming_assistant_buffer);
            let lines = render_markdown(&s, self.config.tui.wrap_width, self.ascii());
            let styled = apply_bar_and_dim(lines, Some(Style::default().fg(Color::Blue)), false, bar_char);
            all_lines.extend(styled);
        }
        self.chat_lines = all_lines;
        self.segment_line_ranges = ranges;
        self.chat_raw.clear();
        self.chat_raw.push_str("[built from segments]");
    }

    async fn rerender_chat(&mut self) {
        // Update Neovim buffer if available
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let content = format_conversation(
                &self.chat_segments,
                &self.streaming_assistant_buffer,
                &self.tool_args_cache,
            );
            
            tracing::debug!("Neovim buffer update: {} chars, {} segments", content.len(), self.chat_segments.len());
            if content.len() < 1000 {
                tracing::debug!("Buffer content:\n{}", content);
            }
            
            let mut bridge = nvim_bridge.lock().await;
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
        }
        
        // Still build display for fallback/search
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

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset =
            (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
    }

    /// Segment index that contains the given line (0-based). Returns None if line is in streaming buffer.
    fn segment_at_line(&self, line: usize) -> Option<usize> {
        self.segment_line_ranges
            .iter()
            .position(|&(start, end)| line >= start && line < end)
    }

    /// If the segment at index i is an editable message (User or Assistant text), return its text.
    fn segment_editable_text(segments: &[ChatSegment], i: usize) -> Option<String> {
        let seg = segments.get(i)?;
        match seg {
            ChatSegment::Message(m) => match (&m.role, &m.content) {
                (Role::User, MessageContent::Text(t)) => Some(t.clone()),
                (Role::Assistant, MessageContent::Text(t)) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Messages for resubmit: only Message segments (no ContextCompacted, no Error).
    fn messages_for_resubmit(segments: &[ChatSegment]) -> Vec<Message> {
        segments
            .iter()
            .filter_map(|s| match s {
                ChatSegment::Message(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    /// When in edit mode, apply Input* action to (edit_buffer, edit_cursor). Returns Some((new_buf, new_cur)) if the action was consumed.
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
            Action::InputMoveCursorLeft => cur = prev_char_boundary(&buf, cur),
            Action::InputMoveCursorRight => {
                if cur < buf.len() {
                    let ch = buf[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    cur += ch;
                }
            }
            Action::InputMoveWordLeft => cur = prev_word_boundary(&buf, cur),
            Action::InputMoveWordRight => cur = next_word_boundary(&buf, cur),
            Action::InputMoveLineStart => cur = 0,
            Action::InputMoveLineEnd => cur = buf.len(),
            Action::InputDeleteToEnd => buf.truncate(cur),
            Action::InputDeleteToStart => {
                buf = buf[cur..].to_string();
                cur = 0;
            }
            _ => return None,
        }
        Some((buf, cur))
    }
}

// ── Character boundary helpers ─────────────────────────────────────────────────

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 { return 0; }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) { p -= 1; }
    p
}

fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[..pos];
    let trimmed = bytes.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    bytes[..trimmed].iter().rposition(|&b| b == b' ').map(|i| i + 1).unwrap_or(0)
}

fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[pos..];
    let start = bytes.iter().position(|&b| b != b' ').unwrap_or(0);
    let end = bytes[start..].iter().position(|&b| b == b' ').unwrap_or(bytes.len() - start);
    pos + start + end
}

// ── Tool call formatting helpers ──────────────────────────────────────────────

const TOOL_CALL_MAX_LINES: usize = 8;

/// Build a markdown output block with middle-truncation for long outputs.
fn format_output_block(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    
    let formatted = if lines.len() <= TOOL_CALL_MAX_LINES {
        lines.join("\n")
    } else {
        let head = TOOL_CALL_MAX_LINES / 2;
        let tail = TOOL_CALL_MAX_LINES - head - 1;
        let omitted = lines.len() - head - tail;
        let head_str = lines[..head].join("\n");
        let tail_str = lines[lines.len() - tail..].join("\n");
        format!("{}\n\n… (+{} lines omitted)\n\n{}", head_str, omitted, tail_str)
    };
    
    format!("\n```\n{}\n```", formatted)
}

/// Format a JSON args value into a short preview string.
fn format_args_preview(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .take(2)
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => truncate_str(s, 40),
                        other => truncate_str(&other.to_string(), 40),
                    };
                    format!("{k}:{val}")
                })
                .collect();
            truncate_str(&parts.join(" "), 80)
        }
        other => truncate_str(&other.to_string(), 80),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let s = s.trim_matches('"');
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Format todo items as markdown for conversation display
fn format_todos_markdown(todos: &[TodoItem]) -> String {
    let mut result = String::from("\n**Todo List Updated:**\n\n");
    
    for todo in todos {
        let _status_icon = match todo.status.to_lowercase().as_str() {
            "completed" => "✓",
            "in_progress" => "▶",
            "pending" => "○",
            "cancelled" => "✗",
            _ => "•",
        };
        
        result.push_str(&format!(
            "- **{}**: {} (id: {})\n",
            todo.status.to_uppercase(),
            todo.content,
            todo.id
        ));
    }
    
    result.push('\n');
    result
}

/// Format a single chat segment as markdown for display.
fn segment_to_markdown(seg: &ChatSegment, tool_args_cache: &HashMap<String, String>) -> String {
    match seg {
        ChatSegment::Message(m) => message_to_markdown(m, tool_args_cache),
        ChatSegment::ContextCompacted {
            tokens_before,
            tokens_after,
        } => format!(
            "\n---\n*Context compacted: {} → {} tokens*\n\n",
            tokens_before, tokens_after
        ),
        ChatSegment::Error(msg) => format!("\n**Error**: {msg}\n\n"),
    }
}

/// Format the entire conversation as markdown for Neovim buffer
fn format_conversation(
    segments: &[ChatSegment],
    streaming_buffer: &str,
    tool_args_cache: &HashMap<String, String>,
) -> String {
    let mut result = String::new();
    
    for (i, seg) in segments.iter().enumerate() {
        let md = segment_to_markdown(seg, tool_args_cache);
        
        // Don't add leading newline for first segment
        if i == 0 && md.starts_with('\n') {
            result.push_str(md.trim_start_matches('\n'));
        } else {
            result.push_str(&md);
        }
    }
    
    // Add streaming buffer if present (only add prefix if not empty conversation)
    if !streaming_buffer.is_empty() {
        if !result.is_empty() {
            result.push_str("**Agent:** ");
        } else {
            result.push_str("**Agent:** ");
        }
        result.push_str(streaming_buffer);
    }
    
    result
}

/// Return (bar_style, dim) for a segment: User = green + dim, Assistant text = blue, else none.
fn segment_bar_style(seg: &ChatSegment) -> (Option<Style>, bool) {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::User, MessageContent::Text(_)) => {
                (Some(Style::default().fg(Color::Green)), true)
            }
            (Role::Assistant, MessageContent::Text(_)) => {
                (Some(Style::default().fg(Color::Blue)), false)
            }
            _ => (None, false),
        },
        _ => (None, false),
    }
}

/// Prepend a bar to each line and optionally apply dim to content.
fn apply_bar_and_dim(
    lines: StyledLines,
    bar_style: Option<Style>,
    dim: bool,
    bar_char: &str,
) -> StyledLines {
    let modifier = if dim { Modifier::DIM } else { Modifier::empty() };
    lines
        .into_iter()
        .map(|line| {
            let mut spans = Vec::new();
            if let Some(style) = bar_style {
                spans.push(Span::styled(bar_char.to_string(), style));
            }
            for s in line.spans {
                spans.push(Span::styled(
                    s.content.to_string(),
                    s.style.patch(Style::default().add_modifier(modifier)),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn message_to_markdown(m: &Message, tool_args_cache: &HashMap<String, String>) -> String {
    use sven_model::Role;
    match (&m.role, &m.content) {
        (Role::User, MessageContent::Text(t)) => format!("---\n\n**You:** {}\n", t),
        (Role::Assistant, MessageContent::Text(t)) => format!("\n**Agent:** {}\n", t),
        (Role::Assistant, MessageContent::ToolCall { function, .. }) => {
            let args_preview = serde_json::from_str(&function.arguments)
                .ok()
                .as_ref()
                .map(format_args_preview)
                .unwrap_or_else(|| truncate_str(&function.arguments, 80));
            format!("\n🔧 **Tool Call: {}**\n```\n{}\n```\n", function.name, args_preview)
        }
        (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
            let tool_name = tool_args_cache
                .get(tool_call_id)
                .and_then(|s| s.split(':').next())
                .unwrap_or("tool");
            let output_block = format_output_block(content);
            format!("✅ **Tool Response: {}**{}\n", tool_name, output_block)
        }
        _ => String::new(),
    }
}

// ── Background agent task ─────────────────────────────────────────────────────

async fn agent_task(
    config: Arc<Config>,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
) {
    let model = match sven_model::from_config(&config.model) {
        Ok(m) => Arc::from(m),
        Err(e) => {
            let _ = tx.send(AgentEvent::Error(format!("model init: {e}"))).await;
            return;
        }
    };

    let mut registry = ToolRegistry::new();
    registry.register(ShellTool { timeout_secs: config.tools.timeout_secs });
    registry.register(FsTool);
    registry.register(GlobTool);
    registry.register(AskQuestionTool::new_tui(question_tx));

    let mut agent = Agent::new(
        model,
        Arc::new(registry),
        Arc::new(config.agent.clone()),
        mode,
        128_000,
    );

    while let Some(req) = rx.recv().await {
        match req {
            AgentRequest::Submit(msg) => {
                debug!(msg_len = msg.len(), "agent task received message");
                if let Err(e) = agent.submit(&msg, tx.clone()).await {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
            AgentRequest::Resubmit {
                messages,
                new_user_content,
            } => {
                debug!("agent task received resubmit");
                if let Err(e) = agent
                    .replace_history_and_submit(messages, &new_user_content, tx.clone())
                    .await
                {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use sven_model::{Message, MessageContent, Role};

    use super::*;

    // ─────────────────────────────────────────────────────────────────────────
    // Shared test helpers
    // ─────────────────────────────────────────────────────────────────────────

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn user_seg(text: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::User,
            content: MessageContent::Text(text.into()),
        })
    }

    fn agent_seg(text: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Assistant,
            content: MessageContent::Text(text.into()),
        })
    }

    fn tool_call_seg(call_id: &str, name: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: call_id.into(),
                function: sven_model::FunctionCall {
                    name: name.into(),
                    arguments: "{}".into(),
                },
            },
        })
    }

    fn tool_result_seg(call_id: &str, output: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: call_id.into(),
                content: output.into(),
            },
        })
    }

    // ─────────────────────────────────────────────────────────────────────────
    // message_to_markdown
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn user_message_formatted_with_separator_and_you_label() {
        // Arrange
        let msg   = Message { role: Role::User, content: MessageContent::Text("hello world".into()) };
        let cache = HashMap::new();

        // Act
        let md = message_to_markdown(&msg, &cache);

        // Assert
        assert!(md.starts_with("---"),       "must start with --- separator; got: {:?}", md);
        assert!(md.contains("**You:**"),     "must carry **You:** label");
        assert!(md.contains("hello world"), "must contain the user text");
        assert!(!md.starts_with('\n'),       "separator must be the first character, no leading newline");
    }

    #[test]
    fn agent_message_formatted_with_agent_label() {
        // Arrange
        let msg   = Message { role: Role::Assistant, content: MessageContent::Text("response text".into()) };
        let cache = HashMap::new();

        // Act
        let md = message_to_markdown(&msg, &cache);

        // Assert
        assert!(md.contains("**Agent:**"),    "must carry **Agent:** label");
        assert!(md.contains("response text"), "must contain the agent text");
    }

    #[test]
    fn tool_call_formatted_with_tool_call_heading_and_name_appears_once() {
        // Arrange
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "id1".into(),
                function: sven_model::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"/tmp/x"}"#.into(),
                },
            },
        };
        let cache = HashMap::new();

        // Act
        let md = message_to_markdown(&msg, &cache);

        // Assert
        assert!(md.contains("Tool Call"),  "must carry 'Tool Call' heading");
        assert!(md.contains("read_file"), "must include the tool name");
        let name_count = md.matches("read_file").count();
        assert_eq!(name_count, 1,
            "tool name must appear exactly once (not duplicated); found {name_count} in: {md:?}");
    }

    #[test]
    fn tool_result_formatted_with_response_heading_output_and_name_appears_once() {
        // Arrange
        let mut cache = HashMap::new();
        cache.insert("id1".to_string(), "read_file".to_string());
        let msg = Message {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id: "id1".into(),
                content: "file contents here".into(),
            },
        };

        // Act
        let md = message_to_markdown(&msg, &cache);

        // Assert
        assert!(md.contains("Tool Response"),     "must carry 'Tool Response' heading");
        assert!(md.contains("file contents here"), "must include the tool output");
        assert!(md.contains("```"),                "output must be inside a code fence");
        let name_count = md.matches("read_file").count();
        assert_eq!(name_count, 1,
            "tool name must appear exactly once; found {name_count} in: {md:?}");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // format_conversation
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_conversation_produces_empty_output() {
        // Arrange
        let segments = Vec::<ChatSegment>::new();
        let cache    = HashMap::new();

        // Act
        let result = format_conversation(&segments, "", &cache);

        // Assert
        assert!(result.trim().is_empty(), "empty conversation must produce empty string; got: {result:?}");
    }

    #[test]
    fn single_user_message_starts_without_leading_newline() {
        // Arrange
        let segments = vec![user_seg("hello")];
        let cache    = HashMap::new();

        // Act
        let result = format_conversation(&segments, "", &cache);

        // Assert
        assert!(result.starts_with("---"),  "must begin with --- separator; got: {result:?}");
        assert!(!result.starts_with('\n'), "must not have a leading newline");
    }

    #[test]
    fn conversation_with_user_and_agent_contains_both_labels_and_texts() {
        // Arrange
        let segments = vec![user_seg("question"), agent_seg("answer")];
        let cache    = HashMap::new();

        // Act
        let result = format_conversation(&segments, "", &cache);

        // Assert
        assert!(result.contains("**You:**"),   "You label present");
        assert!(result.contains("question"),   "user text present");
        assert!(result.contains("**Agent:**"), "Agent label present");
        assert!(result.contains("answer"),     "agent text present");
    }

    #[test]
    fn multi_turn_conversation_has_no_triple_newlines() {
        // Arrange — four alternating messages produce potentially many blank lines
        let segments = vec![user_seg("a"), agent_seg("b"), user_seg("c"), agent_seg("d")];
        let cache    = HashMap::new();

        // Act
        let result = format_conversation(&segments, "", &cache);

        // Assert
        assert!(!result.contains("\n\n\n"),
            "triple consecutive newlines must not appear; got:\n{result}");
    }

    #[test]
    fn streaming_buffer_appended_after_all_committed_segments() {
        // Arrange
        let segments = vec![user_seg("hello")];
        let cache    = HashMap::new();

        // Act
        let result = format_conversation(&segments, "partial response", &cache);

        // Assert
        assert!(result.contains("hello"),            "committed segment present");
        assert!(result.contains("partial response"), "streaming buffer present");
        let user_pos   = result.find("hello").unwrap();
        let stream_pos = result.find("partial response").unwrap();
        assert!(stream_pos > user_pos, "streaming text must come after the committed segment");
    }

    #[test]
    fn tool_call_and_result_both_appear_with_name_at_most_twice() {
        // Arrange
        let mut cache = HashMap::new();
        cache.insert("id1".to_string(), "glob".to_string());
        let segments = vec![
            user_seg("find files"),
            tool_call_seg("id1", "glob"),
            tool_result_seg("id1", "result.txt"),
        ];

        // Act
        let result = format_conversation(&segments, "", &cache);

        // Assert
        assert!(result.contains("glob"),       "tool name must appear");
        assert!(result.contains("result.txt"), "tool output must appear");
        let name_count = result.matches("glob").count();
        assert!(name_count <= 2,
            "tool name must appear at most twice (call + response); found {name_count}:\n{result}");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // format_output_block
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_output_produces_empty_string() {
        // Arrange / Act / Assert
        assert_eq!(format_output_block(""), "");
    }

    #[test]
    fn short_output_wrapped_in_code_fence_with_all_lines_present() {
        // Arrange
        let output = "line1\nline2\nline3";

        // Act
        let result = format_output_block(output);

        // Assert
        assert!(result.contains("```"),   "must use code fence");
        assert!(result.contains("line1"), "line 1 present");
        assert!(result.contains("line2"), "line 2 present");
        assert!(result.contains("line3"), "line 3 present");
    }

    #[test]
    fn long_output_truncated_with_head_tail_and_omission_notice() {
        // Arrange — 30 lines, well above TOOL_CALL_MAX_LINES
        let output: String = (0..30).map(|i| format!("line{i}\n")).collect();

        // Act
        let result = format_output_block(&output);

        // Assert
        assert!(result.contains("omitted"), "omission notice must appear for long output");
        assert!(result.contains("```"),     "code fence still present");
        assert!(result.contains("line0"),   "head of output present");
        assert!(result.contains("line29"),  "tail of output present");
    }

    #[test]
    fn output_at_exactly_max_lines_not_truncated() {
        // Arrange
        let output: String = (0..TOOL_CALL_MAX_LINES).map(|i| format!("line{i}\n")).collect();

        // Act
        let result = format_output_block(&output);

        // Assert
        assert!(!result.contains("omitted"),
            "output at exactly max lines must not show an omission notice");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // is_reserved_key
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn pane_switch_prefix_ctrl_w_is_reserved() {
        // Arrange
        let event = press(KeyCode::Char('w'), KeyModifiers::CONTROL);
        // Act / Assert
        assert!(is_reserved_key(&event), "Ctrl+W must be reserved for pane-switch prefix");
    }

    #[test]
    fn quit_ctrl_c_is_reserved() {
        // Arrange
        let event = press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        // Act / Assert
        assert!(is_reserved_key(&event), "Ctrl+C must be reserved for quit");
    }

    #[test]
    fn pager_ctrl_t_is_reserved() {
        // Arrange
        let event = press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        // Act / Assert
        assert!(is_reserved_key(&event), "Ctrl+T must be reserved for pager");
    }

    #[test]
    fn help_f1_is_reserved() {
        // Arrange
        let event = press(KeyCode::F(1), KeyModifiers::NONE);
        // Act / Assert
        assert!(is_reserved_key(&event), "F1 must be reserved for help");
    }

    #[test]
    fn mode_cycle_f4_is_reserved() {
        // Arrange
        let event = press(KeyCode::F(4), KeyModifiers::NONE);
        // Act / Assert
        assert!(is_reserved_key(&event), "F4 must be reserved for mode cycle");
    }

    #[test]
    fn vim_motion_and_editing_keys_are_not_reserved() {
        // Arrange — characters that Neovim uses for motion and editing
        let vim_keys = ['h', 'j', 'k', 'l', 'i', 'o', 'v', 'G', 'g', 'z', 'Z', 'c', 'd', 'y', 'p'];

        for c in vim_keys {
            // Act
            let event = press(KeyCode::Char(c), KeyModifiers::NONE);
            // Assert
            assert!(!is_reserved_key(&event), "vim key '{c}' must not be reserved");
        }
    }

    #[test]
    fn arrow_and_navigation_keys_are_not_reserved() {
        // Arrange
        let nav_keys = [KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
                        KeyCode::PageUp, KeyCode::PageDown, KeyCode::Home, KeyCode::End];

        for code in nav_keys {
            // Act
            let event = press(code, KeyModifiers::NONE);
            // Assert
            assert!(!is_reserved_key(&event), "{code:?} must not be reserved");
        }
    }

    #[test]
    fn escape_is_not_reserved() {
        // Arrange
        let event = press(KeyCode::Esc, KeyModifiers::NONE);
        // Act / Assert
        assert!(!is_reserved_key(&event), "Esc must not be reserved (Neovim handles it)");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // to_nvim_notation
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn plain_alphabetic_char_passes_through_unchanged() {
        // Arrange
        let cases = [('j', "j"), ('G', "G"), ('i', "i"), ('z', "z")];

        for (c, expected) in cases {
            // Act
            let result = to_nvim_notation(&press(KeyCode::Char(c), KeyModifiers::NONE));
            // Assert
            assert_eq!(result, Some(expected.into()), "char '{c}'");
        }
    }

    #[test]
    fn ctrl_char_encoded_as_angle_bracket_c_notation() {
        // Arrange
        let cases = [('u', "<C-u>"), ('d', "<C-d>"), ('r', "<C-r>"), ('o', "<C-o>")];

        for (c, expected) in cases {
            // Act
            let result = to_nvim_notation(&press(KeyCode::Char(c), KeyModifiers::CONTROL));
            // Assert
            assert_eq!(result, Some(expected.into()), "Ctrl+{c}");
        }
    }

    #[test]
    fn special_keys_encoded_with_angle_bracket_names() {
        // Arrange
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::Esc,       "<Esc>"),
            (KeyCode::Enter,     "<CR>"),
            (KeyCode::Backspace, "<BS>"),
            (KeyCode::Delete,    "<Del>"),
            (KeyCode::Tab,       "<Tab>"),
        ];

        for (code, expected) in cases {
            // Act
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            // Assert
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }

    #[test]
    fn directional_keys_encoded_with_direction_names() {
        // Arrange
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::Up,    "<Up>"),
            (KeyCode::Down,  "<Down>"),
            (KeyCode::Left,  "<Left>"),
            (KeyCode::Right, "<Right>"),
        ];

        for (code, expected) in cases {
            // Act
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            // Assert
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }

    #[test]
    fn page_and_function_keys_encoded_correctly() {
        // Arrange
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::PageUp,   "<PageUp>"),
            (KeyCode::PageDown, "<PageDown>"),
            (KeyCode::F(1),     "<F1>"),
            (KeyCode::F(5),     "<F5>"),
            (KeyCode::F(12),    "<F12>"),
        ];

        for (code, expected) in cases {
            // Act
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            // Assert
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }
}
