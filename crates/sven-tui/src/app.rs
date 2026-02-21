use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
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

/// One segment in the chat display (message or display-only note).
#[derive(Debug, Clone)]
pub enum ChatSegment {
    Message(Message),
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
    },
    Error(String),
    /// Chain-of-thought / extended thinking content from the model.
    /// Collapsed by default with a "Thought" header; expandable in both
    /// Neovim (za) and ratatui (click) modes.
    Thinking { content: String },
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
    /// Pre-load conversation history (resume flow).  Does not trigger a
    /// model call; the agent is just primed for the next submission.
    LoadHistory(Vec<Message>),
}

// ── Key filtering for Neovim forwarding ──────────────────────────────────────

/// Check if a key event is reserved for sven (not forwarded to Neovim)
fn is_reserved_key(event: &KeyEvent) -> bool {
    matches!(
        (event.modifiers, event.code),
        (KeyModifiers::CONTROL, KeyCode::Char('w'))  // Pane switching prefix
        | (KeyModifiers::CONTROL, KeyCode::Char('t'))  // Pager toggle
        | (KeyModifiers::CONTROL, KeyCode::Enter)  // Submit buffer to agent
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
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                "<C-CR>".to_string()
            } else if event.modifiers.contains(KeyModifiers::SHIFT) {
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
    /// Set of segment indices that are collapsed (tool calls/results/thinking).
    /// Only used in ratatui-only mode (no_nvim); Neovim uses its own fold state.
    collapsed_segments: std::collections::HashSet<usize>,
    /// When set, we're in edit mode: edit_buffer/edit_cursor are active.
    editing_message_index: Option<usize>,
    edit_buffer: String,
    edit_cursor: usize,
    /// Original text before editing (used for cancel/restore).
    edit_original_text: Option<String>,
    /// Embedded Neovim instance for chat view
    nvim_bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
    /// Shared with NvimBridge's NvimHandler.  Notified after every Neovim
    /// `flush` event so the render loop can re-draw without waiting for a
    /// keyboard event.  This fixes the "G needs a second keypress" timing bug.
    nvim_flush_notify: Option<Arc<tokio::sync::Notify>>,
    /// Shared with NvimBridge's NvimHandler.  Notified when Neovim sends
    /// "sven_submit" (e.g. from :w command), triggering buffer submission.
    nvim_submit_notify: Option<Arc<tokio::sync::Notify>>,
    /// Notified when Neovim sends "sven_quit" (from :q or :qa command).
    nvim_quit_notify: Option<Arc<tokio::sync::Notify>>,
    /// Path to the current conversation's history file.  Set on first save,
    /// or pre-set when resuming an existing conversation.
    history_path: Option<PathBuf>,
    /// If true, Neovim was disabled (--no-nvim); use ratatui-only chat view.
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
            needs_agent_prefix: false,
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
        // In ratatui-only mode, pre-collapse all tool call/result/thinking segments
        // loaded from initial history so existing conversations start compact.
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
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(4);

        self.agent_tx = Some(submit_tx.clone());
        self.event_rx = Some(event_rx);

        let cfg = self.config.clone();
        let mode = self.mode;

        tokio::spawn(async move {
            agent_task(cfg, mode, submit_rx, event_tx, question_tx).await;
        });

        // When resuming a conversation, prime the agent with the loaded history.
        if !self.chat_segments.is_empty() {
            let messages: Vec<Message> = self
                .chat_segments
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
            self.scroll_to_bottom();
        }

        // Initialize NvimBridge unless disabled by --no-nvim
        if !self.no_nvim {
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
                    // Grab the notifies BEFORE moving the bridge into the Arc.
                    self.nvim_flush_notify = Some(bridge.flush_notify.clone());
                    self.nvim_submit_notify = Some(bridge.submit_notify.clone());
                    self.nvim_quit_notify = Some(bridge.quit_notify.clone());
                    self.nvim_bridge = Some(Arc::new(tokio::sync::Mutex::new(bridge)));
                }
                Err(e) => {
                    tracing::error!("Failed to spawn Neovim: {}. Chat view will be degraded.", e);
                    // Continue without Neovim - we'll handle this gracefully
                }
            }

            // When resuming, the first rerender_chat() ran before the bridge existed, so the
            // Neovim buffer was never filled. Sync the loaded conversation into the buffer now.
            if self.nvim_bridge.is_some() && !self.chat_segments.is_empty() {
                self.rerender_chat().await;
                self.scroll_to_bottom();
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

            // Render Neovim grid to lines before drawing (if available).
            //
            // IMPORTANT: always pass scroll=0 and bridge.height (not self.scroll_offset
            // / self.chat_height).  The Neovim grid IS the viewport — Neovim has
            // already applied its own internal scrolling via grid_scroll events.
            // Passing scroll_offset here would:
            //   1. Cut off the top scroll_offset rows of the Neovim grid
            //   2. Combine with draw_chat's own .skip(scroll_offset) for double-scroll
            //
            // When Neovim is active, draw_chat is called with nvim_draw_scroll=0
            // so it does not skip any rows from the already-correct grid.
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
                // Use Neovim lines if available, otherwise fall back to chat_lines.
                // When Neovim is active, nvim_draw_scroll=0 (no additional skipping —
                // Neovim already managed its own viewport).
                let lines_to_draw = if !nvim_lines.is_empty() {
                    &nvim_lines
                } else {
                    &self.chat_lines
                };

                draw_chat(
                    frame,
                    layout.chat_pane,
                    lines_to_draw,
                    nvim_draw_scroll,
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

            // Clone the Arcs (cheap) so the futures don't borrow self
            // at the same time as the mutable arms below.
            let flush_notify_clone = self.nvim_flush_notify.clone();
            let submit_notify_clone = self.nvim_submit_notify.clone();
            let quit_notify_clone = self.nvim_quit_notify.clone();
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
                // Re-render when Neovim finishes a redraw cycle (flush event).
                // This ensures the grid and cursor state are always current
                // without waiting for the next user keypress.
                _ = Self::nvim_notify_future(flush_notify_clone.as_deref()) => {}
                // Submit buffer when Neovim sends "sven_submit" (from :w command).
                _ = Self::nvim_notify_future(submit_notify_clone.as_deref()) => {
                    let _ = self.dispatch(Action::SubmitBufferToAgent).await;
                }
                // Quit when Neovim sends "sven_quit" (from :q or :qa command).
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

    /// Returns a future that resolves when the given notify fires, or never
    /// resolves if notify is None. Used for Neovim flush and submit events.
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
                if self.needs_agent_prefix {
                    self.needs_agent_prefix = false;
                }
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
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
                self.nvim_scroll_to_bottom().await;
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
                // In ratatui-only mode, default tool call segments to collapsed.
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
                self.chat_segments
                    .push(ChatSegment::Message(Message::tool_result(&call_id, &output)));
                // In ratatui-only mode, default tool result segments to collapsed.
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
                self.save_history_async();
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
            // Accumulate streaming thinking deltas into a temporary buffer.
            // The buffer is prepended to the next streaming render pass so the
            // user can see thinking content arrive in real time.
            AgentEvent::ThinkingDelta(delta) => {
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
            }
            // A complete thinking block arrived: store it as a Thinking segment.
            // In ratatui-only mode it starts collapsed; Neovim uses fold level 1.
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
                        MouseEventKind::ScrollUp => {
                            // When Neovim is active it owns the viewport; forward
                            // scroll as Neovim commands instead of moving scroll_offset.
                            if self.nvim_bridge.is_some() {
                                if let Some(nvim_bridge) = &self.nvim_bridge {
                                    let mut bridge = nvim_bridge.lock().await;
                                    // <C-y> scrolls viewport up (reveals earlier lines)
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
                                    // <C-e> scrolls viewport down (reveals later lines)
                                    let _ = bridge.send_input("<C-e><C-e><C-e>").await;
                                }
                            } else {
                                self.scroll_down(3);
                            }
                        }
                        // Left click toggles collapse for tool call/result segments
                        // (ratatui-only mode; Neovim handles folds via its own fold toggle).
                        MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            if self.no_nvim =>
                        {
                            // Layout: row 0 = status bar, row 1 = chat border top,
                            // rows 2..N-1 = chat content, row N = chat border bottom.
                            // Map click row to logical line index in chat_lines.
                            let content_start_row: u16 = 2; // status(1) + border(1)
                            if mouse.row >= content_start_row {
                                let click_line = (mouse.row - content_start_row) as usize
                                    + self.scroll_offset as usize;
                                if let Some(seg_idx) = self.segment_at_line(click_line) {
                                    if let Some(seg) = self.chat_segments.get(seg_idx) {
                                        // Check if it's an editable message (User or Assistant text)
                                        let is_editable = Self::segment_editable_text(&self.chat_segments, seg_idx).is_some();
                                        
                                        if is_editable {
                                            // Start editing this message
                                            if let Some(text) = Self::segment_editable_text(&self.chat_segments, seg_idx) {
                                                self.editing_message_index = Some(seg_idx);
                                                self.edit_cursor = text.len();
                                                self.edit_original_text = Some(text.clone());
                                                self.edit_buffer = text;
                                                self.focus = FocusPane::Input; // Switch focus to input for editing
                                                self.update_editing_segment_live();
                                                self.rerender_chat().await;
                                            }
                                        } else {
                                            // Check if it's collapsible (tool call/result/thinking)
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
                if let Some(seg_idx) = self.segment_at_line(line) {
                    if let Some(text) = Self::segment_editable_text(&self.chat_segments, seg_idx) {
                        self.editing_message_index = Some(seg_idx);
                        self.edit_cursor = text.len();
                        self.edit_original_text = Some(text.clone());
                        self.edit_buffer = text;
                        self.focus = FocusPane::Input; // Switch focus to input for editing
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
                            // User message: truncate below, update message, and resubmit
                            self.chat_segments.truncate(i + 1);
                            self.chat_segments.pop();
                            self.chat_segments.push(ChatSegment::Message(Message::user(&new_content)));
                            let messages = Self::messages_for_resubmit(&self.chat_segments);
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, new_content).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            // Assistant message: just update in place, no resubmit
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
                // Restore the original text before canceling
                if let Some(idx) = self.editing_message_index {
                    if let Some(original) = &self.edit_original_text {
                        if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(idx) {
                            match (&m.role, &mut m.content) {
                                (Role::User, MessageContent::Text(t)) => {
                                    *t = original.clone();
                                }
                                (Role::Assistant, MessageContent::Text(t)) => {
                                    *t = original.clone();
                                }
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
                // Get content from Neovim buffer, parse to messages, and resubmit
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
                    
                    // Parse markdown to messages
                    match parse_markdown_to_messages(&markdown) {
                        Ok(messages) => {
                            if messages.is_empty() {
                                tracing::warn!("Empty buffer, nothing to submit");
                                return false;
                            }
                            
                            // Find the last user message as the new_user_content
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
                            
                            // Update our internal state with parsed messages
                            self.chat_segments = messages
                                .iter()
                                .map(|m| ChatSegment::Message(m.clone()))
                                .collect();
                            
                            // Rebuild tool_args_cache for tool result display
                            self.tool_args_cache.clear();
                            for msg in &messages {
                                if let MessageContent::ToolCall { tool_call_id, function } = &msg.content {
                                    self.tool_args_cache
                                        .insert(tool_call_id.clone(), function.name.clone());
                                }
                            }
                            
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, new_user_content).await;
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse buffer markdown: {}", e);
                            // TODO: Show error in UI?
                            return false;
                        }
                    }
                } else {
                    tracing::warn!("SubmitBufferToAgent called but nvim_bridge not available");
                }
            }

            // When Neovim is active it manages the viewport; forward scroll
            // actions as Neovim scroll commands.  When it is not active, use
            // the normal TUI scroll_offset mechanism.
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
                // /quit in input pane exits the application
                if text.eq_ignore_ascii_case("/quit") {
                    return true;
                }
                if !text.is_empty() {
                    if self.agent_busy {
                        self.queued.push_back(text);
                    } else {
                        // Sync any in-buffer edits the user made before appending
                        // the new message.  Without this, rerender_chat() would
                        // overwrite the buffer from the stale chat_segments.
                        self.sync_nvim_buffer_to_segments().await;
                        // Capture the (potentially edited) history so we can
                        // send it to the agent via Resubmit.  This ensures that
                        // any changes the user made in the Neovim buffer are
                        // reflected in the model's conversation context.  Using
                        // plain Submit would only append to the agent's stale
                        // internal history, ignoring the edits.
                        let history = Self::messages_for_resubmit(&self.chat_segments);
                        self.chat_segments
                            .push(ChatSegment::Message(Message::user(&text)));
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
            // Note: rerender_chat() will set buffer to non-modifiable during streaming
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
            // Note: rerender_chat() will set buffer to non-modifiable during streaming
        }
    }


    /// Build chat_lines and segment_line_ranges from chat_segments and streaming buffer.
    /// User messages get a green vertical bar and dimmed text; agent messages get a blue bar.
    /// Tool call/result segments get an orange bar.  In ratatui-only mode (no_nvim), segments
    /// listed in `collapsed_segments` are rendered as a single summary line.
    fn build_display_from_segments(&mut self) {
        let mut all_lines = Vec::new();
        let mut ranges = Vec::new();
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
            
            let mut bridge = nvim_bridge.lock().await;
            
            // Temporarily make buffer modifiable for updates
            if let Err(e) = bridge.set_modifiable(true).await {
                tracing::error!("Failed to set buffer modifiable for update: {}", e);
            }
            
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
            
            // Set back to non-modifiable if agent is still busy (streaming)
            if self.agent_busy {
                if let Err(e) = bridge.set_modifiable(false).await {
                    tracing::error!("Failed to set buffer non-modifiable: {}", e);
                }
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

    /// Collect `Message` objects from `chat_segments` and persist the
    /// conversation to disk.  Runs the I/O in a background task so it does
    /// not block the event loop.  Errors are logged and silently swallowed.
    fn save_history_async(&mut self) {
        let messages: Vec<sven_model::Message> = self
            .chat_segments
            .iter()
            .filter_map(|seg| {
                if let ChatSegment::Message(m) = seg {
                    Some(m.clone())
                } else {
                    None
                }
            })
            .collect();

        if messages.is_empty() {
            return;
        }

        let path_opt = self.history_path.clone();

        // We need to propagate the new path back if this is the first save.
        // Because `tokio::spawn` cannot mutate `self`, we save synchronously
        // for the first save (path creation) and then background subsequent ones.
        match path_opt {
            None => {
                // First save: create the file and record the path.
                match sven_input::history::save(&messages) {
                    Ok(path) => {
                        debug!(path = %path.display(), "conversation saved to history");
                        self.history_path = Some(path);
                    }
                    Err(e) => {
                        debug!("failed to save conversation to history: {e}");
                    }
                }
            }
            Some(path) => {
                // Subsequent saves: overwrite in background.
                tokio::spawn(async move {
                    if let Err(e) = sven_input::history::save_to(&path, &messages) {
                        debug!("failed to update conversation history: {e}");
                    }
                });
            }
        }
    }

    fn scroll_to_bottom(&mut self) {
        // When Neovim is active it owns the viewport.  Modifying scroll_offset
        // here would corrupt the Neovim grid rendering because the offset gets
        // applied on top of Neovim's already-scrolled grid, cutting off content.
        // Callers that also want to scroll Neovim to the bottom must separately
        // call nvim_scroll_to_bottom().
        if self.nvim_bridge.is_none() {
            self.scroll_offset =
                (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
        }
    }

    /// Read the Neovim buffer and update `chat_segments` from it.
    ///
    /// Called before submitting a new user message so that any in-buffer edits
    /// the user made are preserved rather than discarded when `rerender_chat`
    /// overwrites the buffer.  If the buffer cannot be read or does not parse,
    /// the existing `chat_segments` are left unchanged.
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

    /// Send "G" to Neovim to scroll to the end of the buffer.
    /// This is the async counterpart to scroll_to_bottom() for when the nvim
    /// bridge is active.  Errors are silently ignored (non-critical UI update).
    async fn nvim_scroll_to_bottom(&self) {
        if let Some(nvim_bridge) = &self.nvim_bridge {
            let mut bridge = nvim_bridge.lock().await;
            let _ = bridge.send_input("G").await;
        }
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

    /// Update the segment being edited with the current edit_buffer content (live preview).
    fn update_editing_segment_live(&mut self) {
        if let Some(idx) = self.editing_message_index {
            if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(idx) {
                match (&m.role, &mut m.content) {
                    (Role::User, MessageContent::Text(t)) => {
                        *t = self.edit_buffer.clone();
                    }
                    (Role::Assistant, MessageContent::Text(t)) => {
                        *t = self.edit_buffer.clone();
                    }
                    _ => {}
                }
            }
            self.build_display_from_segments();
            self.search.update_matches(&self.chat_lines);
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


// ── Legacy formatting helpers (used only in tests for validating old behavior) ───

#[cfg(test)]
const TOOL_CALL_MAX_LINES: usize = 8;

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
        ChatSegment::Thinking { content } => {
            format!("\n**Agent:thinking**\n💭 **Thought**\n```\n{}\n```\n", content)
        }
    }
}

/// Render a single-line collapsed preview for a segment (ratatui-only mode).
/// Shows the tool name and a short content preview; user clicks to expand.
fn collapsed_preview(seg: &ChatSegment, tool_args_cache: &HashMap<String, String>) -> String {
    use sven_model::Role;
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::Assistant, MessageContent::ToolCall { tool_call_id, function }) => {
                // Show tool name and a brief args preview
                let args_preview = serde_json::from_str::<serde_json::Value>(&function.arguments)
                    .map(|v| {
                        if let serde_json::Value::Object(map) = &v {
                            let parts: Vec<String> = map.iter().take(2).map(|(k, val)| {
                                let s = match val {
                                    serde_json::Value::String(s) => s.chars().take(40).collect::<String>(),
                                    other => other.to_string().chars().take(40).collect::<String>(),
                                };
                                format!("{}={}", k, s)
                            }).collect();
                            parts.join(" ")
                        } else {
                            function.arguments.chars().take(60).collect::<String>()
                        }
                    })
                    .unwrap_or_else(|_| function.arguments.chars().take(60).collect::<String>());
                format!(
                    "\n**Agent:tool_call:{}**\n🔧 **Tool Call: {}** `{}` ▶ click to expand\n",
                    tool_call_id, function.name, args_preview
                )
            }
            (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
                let tool_name = tool_args_cache
                    .get(tool_call_id)
                    .map(|s| s.as_str())
                    .unwrap_or("tool");
                let preview: String = content.lines().next().unwrap_or("").chars().take(80).collect();
                let truncated = if content.len() > preview.len() + 1 { "…" } else { "" };
                format!(
                    "\n**Tool:{}**\n✅ **Tool Response: {}** `{}{}` ▶ click to expand\n",
                    tool_call_id, tool_name, preview, truncated
                )
            }
            _ => segment_to_markdown(seg, tool_args_cache),
        },
        ChatSegment::Thinking { content } => {
            let preview: String = content.lines().next().unwrap_or("").chars().take(80).collect();
            let truncated = if content.len() > preview.len() + 1 { "…" } else { "" };
            format!(
                "\n**Agent:thinking**\n💭 **Thought** `{}{}` ▶ click to expand\n",
                preview, truncated
            )
        }
        _ => segment_to_markdown(seg, tool_args_cache),
    }
}

/// Parse markdown buffer back into structured messages for resubmit.
/// This is the inverse of `message_to_markdown`, enabling lossless round-trip editing.
///
/// Format parsed:
/// - `**You:** text` → User message
/// - `**Agent:** text` → Assistant text message
/// - `**Agent:tool_call:ID**` + ```json block → Assistant ToolCall
/// - `**Tool:ID**` + ``` block → Tool ToolResult
/// - `**System:** text` → System message
/// - `---` → Turn separator (informational, not structural)
fn parse_markdown_to_messages(markdown: &str) -> Result<Vec<Message>, String> {
    let mut messages = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;
    
    while i < lines.len() {
        let line = lines[i].trim();
        
        // Skip empty lines and --- separators
        if line.is_empty() || line == "---" {
            i += 1;
            continue;
        }
        
        // Parse role headers: **Role:** or **Role:metadata**
        if let Some(msg) = parse_message_at_line(&lines, &mut i)? {
            messages.push(msg);
        } else {
            i += 1;
        }
    }
    
    Ok(messages)
}

/// Parse a single message starting at lines[*i]. Advances *i past the message.
/// Returns None if no valid message header found at this position.
fn parse_message_at_line(lines: &[&str], i: &mut usize) -> Result<Option<Message>, String> {
    if *i >= lines.len() {
        return Ok(None);
    }
    
    let line = lines[*i].trim();
    
    // Match role headers
    if line.starts_with("**You:**") {
        let text = extract_text_content(lines, i, "**You:**")?;
        return Ok(Some(Message::user(text)));
    }
    
    if line.starts_with("**Agent:tool_call:") {
        // Extract tool_call_id from **Agent:tool_call:ID**
        let tool_call_id = line
            .strip_prefix("**Agent:tool_call:")
            .and_then(|s| s.strip_suffix("**"))
            .ok_or_else(|| format!("Malformed tool_call header: {}", line))?
            .trim()
            .to_string();
        
        *i += 1;
        
        // Skip display line: "🔧 **Tool Call: name**"
        skip_until_code_fence(lines, i);
        
        // Extract JSON args from ```json block
        let arguments = extract_code_block(lines, i)?;
        
        // Extract tool name from the display line we skipped
        let name = extract_tool_name_from_previous_lines(lines, *i)?;
        
        return Ok(Some(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id,
                function: FunctionCall { name, arguments },
            },
        }));
    }
    
    if line.starts_with("**Tool:") {
        // Extract tool_call_id from **Tool:ID**
        let tool_call_id = line
            .strip_prefix("**Tool:")
            .and_then(|s| s.strip_suffix("**"))
            .ok_or_else(|| format!("Malformed tool result header: {}", line))?
            .trim()
            .to_string();
        
        *i += 1;
        
        // Skip display line: "✅ **Tool Response: name**"
        skip_until_code_fence(lines, i);
        
        // Extract output from ``` block
        let content = extract_code_block(lines, i)?;
        
        return Ok(Some(Message {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id,
                content,
            },
        }));
    }
    
    if line.starts_with("**Agent:**") {
        let text = extract_text_content(lines, i, "**Agent:**")?;
        return Ok(Some(Message::assistant(text)));
    }
    
    if line.starts_with("**System:**") {
        let text = extract_text_content(lines, i, "**System:**")?;
        return Ok(Some(Message::system(text)));
    }
    
    Ok(None)
}

/// Extract text content for a simple text message. Advances *i past the message.
fn extract_text_content(lines: &[&str], i: &mut usize, prefix: &str) -> Result<String, String> {
    let first_line = lines[*i].trim();
    let inline_text = first_line
        .strip_prefix(prefix)
        .map(|s| s.trim())
        .unwrap_or("");
    
    let mut text = String::from(inline_text);
    *i += 1;
    
    // Collect subsequent lines until we hit a new header, ---, or end
    while *i < lines.len() {
        let line = lines[*i];
        let trimmed = line.trim();
        
        // Stop at next message header or separator
        if trimmed.starts_with("**") || trimmed == "---" {
            break;
        }
        
        // Accumulate non-empty content lines
        if !trimmed.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(trimmed);
        }
        
        *i += 1;
    }
    
    Ok(text)
}

/// Skip lines until we find a code fence marker (```).
fn skip_until_code_fence(lines: &[&str], i: &mut usize) {
    while *i < lines.len() {
        if lines[*i].trim().starts_with("```") {
            return;
        }
        *i += 1;
    }
}

/// Extract content from a code block. Assumes *i points to the opening ```.
/// Advances *i past the closing ```.
fn extract_code_block(lines: &[&str], i: &mut usize) -> Result<String, String> {
    if *i >= lines.len() || !lines[*i].trim().starts_with("```") {
        return Err(format!("Expected code fence at line {}", i));
    }
    
    *i += 1; // skip opening ```
    
    let mut content = String::new();
    while *i < lines.len() {
        let line = lines[*i];
        if line.trim().starts_with("```") {
            *i += 1; // skip closing ```
            return Ok(content.trim_end().to_string());
        }
        
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(line);
        *i += 1;
    }
    
    Err("Unclosed code block".to_string())
}

/// Extract tool name from the display line "🔧 **Tool Call: name**" that
/// appears before the code block. Looks backward from current position.
/// No fixed line limit: scans until the tool-call header is found or a section
/// separator is hit, so it works correctly with multi-line pretty-printed JSON.
fn extract_tool_name_from_previous_lines(lines: &[&str], current: usize) -> Result<String, String> {
    for j in (0..current).rev() {
        let line = lines[j].trim();
        if let Some(rest) = line.strip_prefix("🔧 **Tool Call:") {
            if let Some(name) = rest.strip_suffix("**") {
                return Ok(name.trim().to_string());
            }
        }
        // Stop at section boundaries so we never bleed into a previous segment.
        if line == "---"
            || line.starts_with("**Agent:")
            || line.starts_with("**You:")
            || line.starts_with("**Tool:")
        {
            break;
        }
    }
    Err("Could not find tool name in previous lines".to_string())
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
    
    // Add streaming buffer if present
    if !streaming_buffer.is_empty() {
        result.push_str("**Agent:** ");
        result.push_str(streaming_buffer);
    }
    
    result
}

/// Return (bar_style, dim) for a segment: User = green + dim, Assistant text = blue, tool calls/results = orange, else none.
fn segment_bar_style(seg: &ChatSegment) -> (Option<Style>, bool) {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::User, MessageContent::Text(_)) => {
                (Some(Style::default().fg(Color::Green)), true)
            }
            (Role::Assistant, MessageContent::Text(_)) => {
                (Some(Style::default().fg(Color::Blue)), false)
            }
            (Role::Assistant, MessageContent::ToolCall { .. }) => {
                (Some(Style::default().fg(Color::Rgb(255, 165, 0))), false)
            }
            (Role::Tool, MessageContent::ToolResult { .. }) => {
                (Some(Style::default().fg(Color::Rgb(255, 165, 0))), false)
            }
            _ => (None, false),
        },
        ChatSegment::Thinking { .. } => {
            // Purple bar for thinking/reasoning content
            (Some(Style::default().fg(Color::Rgb(160, 100, 200))), false)
        }
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
        // User message: starts a new turn with --- separator
        (Role::User, MessageContent::Text(t)) => format!("---\n\n**You:** {}\n", t),
        
        // Assistant text message
        (Role::Assistant, MessageContent::Text(t)) => format!("\n**Agent:** {}\n", t),
        
        // Tool call: store full args as JSON for lossless round-trip.
        // Format: **Agent:tool_call:ID** followed by formatted display.
        (Role::Assistant, MessageContent::ToolCall { tool_call_id, function }) => {
            let pretty_args = serde_json::from_str::<serde_json::Value>(&function.arguments)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_else(|_| function.arguments.clone());
            format!(
                "\n**Agent:tool_call:{}**\n🔧 **Tool Call: {}**\n```json\n{}\n```\n",
                tool_call_id,
                function.name,
                pretty_args
            )
        }
        
        // Tool result: store full output (no truncation) for lossless round-trip.
        // Format: **Tool:ID** followed by formatted display.
        (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
            let tool_name = tool_args_cache
                .get(tool_call_id)
                .map(|s| s.as_str())
                .unwrap_or("tool");
            format!(
                "\n**Tool:{}**\n✅ **Tool Response: {}**\n```\n{}\n```\n",
                tool_call_id,
                tool_name,
                content
            )
        }
        
        // System messages: render with System header for lossless format
        (Role::System, MessageContent::Text(t)) => format!("**System:** {}\n\n", t),
        
        // Fallback for any unexpected combinations
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
            AgentRequest::LoadHistory(messages) => {
                debug!(n = messages.len(), "agent task loading history");
                agent.session_mut().replace_messages(messages);
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
    // parse_markdown_to_messages — round-trip tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_empty_markdown_produces_empty_messages() {
        // Arrange / Act
        let result = parse_markdown_to_messages("").unwrap();
        
        // Assert
        assert!(result.is_empty(), "empty markdown must parse to empty message list");
    }

    #[test]
    fn parse_single_user_message_extracts_role_and_text() {
        // Arrange
        let md = "---\n\n**You:** hello world\n";
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 1, "must parse exactly one message");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].as_text(), Some("hello world"));
    }

    #[test]
    fn parse_user_and_agent_messages_preserves_order_and_content() {
        // Arrange
        let md = "---\n\n**You:** question\n\n**Agent:** answer\n";
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].as_text(), Some("question"));
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].as_text(), Some("answer"));
    }

    #[test]
    fn parse_tool_call_extracts_id_name_and_full_args() {
        // Arrange
        let md = concat!(
            "**Agent:tool_call:abc123**\n",
            "🔧 **Tool Call: read_file**\n",
            "```json\n",
            r#"{"path": "/tmp/test.txt"}"#, "\n",
            "```\n",
        );
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        if let MessageContent::ToolCall { tool_call_id, function } = &messages[0].content {
            assert_eq!(tool_call_id, "abc123");
            assert_eq!(function.name, "read_file");
            assert_eq!(function.arguments.trim(), r#"{"path": "/tmp/test.txt"}"#);
        } else {
            panic!("expected ToolCall content");
        }
    }

    #[test]
    fn parse_tool_result_extracts_id_and_full_output() {
        // Arrange
        let md = concat!(
            "**Tool:xyz789**\n",
            "✅ **Tool Response: glob**\n",
            "```\n",
            "file1.rs\n",
            "file2.rs\n",
            "```\n",
        );
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Tool);
        if let MessageContent::ToolResult { tool_call_id, content } = &messages[0].content {
            assert_eq!(tool_call_id, "xyz789");
            assert_eq!(content.trim(), "file1.rs\nfile2.rs");
        } else {
            panic!("expected ToolResult content");
        }
    }

    #[test]
    fn parse_system_message_extracts_role_and_text() {
        // Arrange
        let md = "**System:** You are a helpful assistant.\n\n";
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].as_text(), Some("You are a helpful assistant."));
    }

    #[test]
    fn roundtrip_user_and_agent_messages_preserves_content() {
        // Arrange
        let original = vec![
            Message::user("first question"),
            Message::assistant("first answer"),
            Message::user("second question"),
        ];
        let cache = HashMap::new();
        
        // Act — convert to markdown and back
        let md: String = original.iter().map(|m| message_to_markdown(m, &cache)).collect();
        let parsed = parse_markdown_to_messages(&md).unwrap();
        
        // Assert
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].as_text(), original[0].as_text());
        assert_eq!(parsed[1].as_text(), original[1].as_text());
        assert_eq!(parsed[2].as_text(), original[2].as_text());
    }

    #[test]
    fn roundtrip_conversation_with_tool_call_and_result_preserves_all_data() {
        // Arrange
        let mut cache = HashMap::new();
        cache.insert("call1".to_string(), "read_file".to_string());
        
        let original = vec![
            Message::user("read the file"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call1".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"/tmp/x","encoding":"utf8"}"#.into(),
                    },
                },
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult {
                    tool_call_id: "call1".into(),
                    content: "file contents\nline two\nline three".into(),
                },
            },
            Message::assistant("The file contains three lines"),
        ];
        
        // Act
        let md: String = original.iter().map(|m| message_to_markdown(m, &cache)).collect();
        let parsed = parse_markdown_to_messages(&md).unwrap();
        
        // Assert — all four messages reconstructed with full data
        assert_eq!(parsed.len(), 4, "all messages must be preserved");
        
        // User message
        assert_eq!(parsed[0].role, Role::User);
        assert_eq!(parsed[0].as_text(), Some("read the file"));
        
        // Tool call with full args (not truncated)
        assert_eq!(parsed[1].role, Role::Assistant);
        if let MessageContent::ToolCall { tool_call_id, function } = &parsed[1].content {
            assert_eq!(tool_call_id, "call1");
            assert_eq!(function.name, "read_file");
            assert!(function.arguments.contains("utf8"),
                "full args must be preserved; got: {}", function.arguments);
        } else {
            panic!("expected ToolCall");
        }
        
        // Tool result with full output (not truncated)
        assert_eq!(parsed[2].role, Role::Tool);
        if let MessageContent::ToolResult { tool_call_id, content } = &parsed[2].content {
            assert_eq!(tool_call_id, "call1");
            assert!(content.contains("line three"),
                "full output must be preserved; got: {}", content);
        } else {
            panic!("expected ToolResult");
        }
        
        // Final assistant message
        assert_eq!(parsed[3].role, Role::Assistant);
        assert_eq!(parsed[3].as_text(), Some("The file contains three lines"));
    }

    #[test]
    fn parse_multiline_user_message_joins_lines() {
        // Arrange
        let md = "**You:** first line\nsecond line\nthird line\n";
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 1);
        let text = messages[0].as_text().unwrap();
        assert!(text.contains("first line"), "first line present");
        assert!(text.contains("second line"), "second line present");
        assert!(text.contains("third line"), "third line present");
    }

    #[test]
    fn parse_stops_at_next_message_header() {
        // Arrange — ensure parser doesn't consume text from the next message
        let md = "**You:** first\n\n**Agent:** second\n";
        
        // Act
        let messages = parse_markdown_to_messages(md).unwrap();
        
        // Assert
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].as_text(), Some("first"));
        assert_eq!(messages[1].as_text(), Some("second"));
    }

    #[test]
    fn edit_first_user_message_then_parse_produces_correct_messages_for_resubmit() {
        // Arrange — simulate editing the first user message in a multi-turn conversation
        let mut cache = HashMap::new();
        cache.insert("c1".to_string(), "glob".to_string());
        
        let original_segments = vec![
            user_seg("ORIGINAL_QUESTION"),
            agent_seg("first answer"),
            tool_call_seg("c1", "glob"),
            tool_result_seg("c1", "file.rs"),
            agent_seg("Found file"),
            user_seg("second question"),
            agent_seg("second answer"),
        ];
        
        // Convert to markdown (as it appears in Neovim)
        let original_md = format_conversation(&original_segments, "", &cache);
        
        // Act — simulate user editing: replace "ORIGINAL_QUESTION" with "EDITED_QUESTION"
        let edited_md = original_md.replace("ORIGINAL_QUESTION", "EDITED_QUESTION");
        
        // Parse edited markdown back to messages
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        
        // Assert — all messages parsed correctly with edited text
        assert_eq!(parsed.len(), 7, "all messages must be present");
        assert_eq!(parsed[0].as_text(), Some("EDITED_QUESTION"), "first message was edited");
        assert_eq!(parsed[1].as_text(), Some("first answer"), "second message unchanged");
        assert_eq!(parsed[5].as_text(), Some("second question"), "later messages unchanged");
        
        // Verify tool call structure preserved
        if let MessageContent::ToolCall { tool_call_id, function } = &parsed[2].content {
            assert_eq!(tool_call_id, "c1");
            assert_eq!(function.name, "glob");
        } else {
            panic!("tool call structure must be preserved");
        }
    }

    #[test]
    fn edit_middle_agent_response_then_parse_truncates_correctly() {
        // Arrange — edit an assistant message in the middle should truncate
        // to the preceding user message for resubmit (standard edit-assistant behavior)
        let original_segments = vec![
            user_seg("question 1"),
            agent_seg("EDIT_THIS_RESPONSE"),
            user_seg("question 2"),
            agent_seg("answer 2"),
        ];
        let cache = HashMap::new();
        let original_md = format_conversation(&original_segments, "", &cache);
        
        // Act — simulate editing the first agent response
        let edited_md = original_md.replace("EDIT_THIS_RESPONSE", "EDITED_RESPONSE");
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        
        // For resubmit after editing assistant message, the app truncates to the
        // last user message before the edit point. Parse gives us all 4 messages;
        // the app's truncation logic (lines 897-916) would then cut after message[0].
        // Here we just verify parsing gives us all messages with edits intact.
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].as_text(), Some("question 1"));
        assert_eq!(parsed[1].as_text(), Some("EDITED_RESPONSE"), "agent message edited");
        assert_eq!(parsed[2].as_text(), Some("question 2"));
        assert_eq!(parsed[3].as_text(), Some("answer 2"));
    }

    #[test]
    fn delete_last_turn_by_removing_markdown_then_parse_produces_truncated_list() {
        // Arrange — simulate deleting the last turn by removing its markdown
        let original_segments = vec![
            user_seg("keep this"),
            agent_seg("keep this too"),
            user_seg("DELETE_ME"),
            agent_seg("delete this also"),
        ];
        let cache = HashMap::new();
        let original_md = format_conversation(&original_segments, "", &cache);
        
        // Act — remove everything after "keep this too"
        let lines: Vec<&str> = original_md.lines().collect();
        let truncated_lines: Vec<&str> = lines
            .iter()
            .take_while(|line| !line.contains("DELETE_ME"))
            .copied()
            .collect();
        let edited_md = truncated_lines.join("\n");
        
        let parsed = parse_markdown_to_messages(&edited_md).unwrap();
        
        // Assert — only first two messages remain
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].as_text(), Some("keep this"));
        assert_eq!(parsed[1].as_text(), Some("keep this too"));
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
    fn ctrl_c_not_reserved() {
        // Quit is via :q/:qa in chat and /quit in input; Ctrl+C is not reserved (forwarded to Neovim in chat)
        let event = press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!is_reserved_key(&event));
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
