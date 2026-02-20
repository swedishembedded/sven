use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
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
use sven_tools::{AskQuestionTool, FsTool, GlobTool, QuestionRequest, ShellTool, ToolRegistry};

use crate::{
    keys::{map_key, Action},
    layout::AppLayout,
    markdown::{render_markdown, StyledLines},
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

        if let Some(p) = self.queued.pop_front() {
            self.chat_segments
                .push(ChatSegment::Message(Message::user(&p)));
            self.rerender_chat();
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
                draw_chat(
                    frame,
                    layout.chat_pane,
                    &self.chat_lines,
                    self.scroll_offset,
                    self.focus == FocusPane::Chat,
                    ascii,
                    &self.search.query,
                    &self.search.matches,
                    self.search.current,
                    self.search.regex.as_ref(),
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
                    if self.handle_agent_event(agent_event) { break; }
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

    fn handle_agent_event(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::TextDelta(delta) => {
                if self.needs_agent_prefix {
                    self.needs_agent_prefix = false;
                }
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat();
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::TextComplete(full_text) => {
                self.chat_segments
                    .push(ChatSegment::Message(Message::assistant(&full_text)));
                self.streaming_assistant_buffer.clear();
                self.rerender_chat();
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallStarted(tc) => {
                let args_preview = format_args_preview(&tc.args);
                self.tool_args_cache.insert(tc.id.clone(), args_preview.clone());
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
                self.rerender_chat();
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallFinished { call_id, output, .. } => {
                self.current_tool = None;
                self.chat_segments
                    .push(ChatSegment::Message(Message::tool_result(&call_id, &output)));
                self.rerender_chat();
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
                self.rerender_chat();
            }
            AgentEvent::TokenUsage { input, output, .. } => {
                let max = 128_000u32;
                self.context_pct = ((input + output) * 100 / max.max(1)).min(100) as u8;
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                self.current_tool = None;
                if let Some(next) = self.queued.pop_front() {
                    let tx = self.agent_tx.clone().unwrap();
                    tokio::spawn(async move { let _ = tx.send(AgentRequest::Submit(next)).await; });
                    self.agent_busy = true;
                    self.needs_agent_prefix = true;
                }
            }
            AgentEvent::Error(msg) => {
                self.chat_segments.push(ChatSegment::Error(msg.clone()));
                self.rerender_chat();
                self.agent_busy = false;
                self.current_tool = None;
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

            Event::Resize(_, _) => {
                self.rerender_chat();
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
                            self.rerender_chat();
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
                            self.rerender_chat();
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
                        self.rerender_chat();
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

    fn rerender_chat(&mut self) {
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
    
    let formatted_lines = if lines.len() <= TOOL_CALL_MAX_LINES {
        lines.iter()
            .map(|l| format!("> {}", l))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let head = TOOL_CALL_MAX_LINES / 2;
        let tail = TOOL_CALL_MAX_LINES - head - 1;
        let omitted = lines.len() - head - tail;
        let head_str = lines[..head].iter()
            .map(|l| format!("> {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        let tail_str = lines[lines.len() - tail..].iter()
            .map(|l| format!("> {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{}\n> \n> … (+{} lines omitted)\n> \n{}", head_str, omitted, tail_str)
    };
    
    format!("\n>\n> **Output:**\n> ```\n{}\n> ```", formatted_lines)
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
        (Role::User, MessageContent::Text(t)) => format!("\n---\n\n> **You:** {t}\n\n"),
        (Role::Assistant, MessageContent::Text(t)) => format!("**Agent:** {t}\n\n"),
        (Role::Assistant, MessageContent::ToolCall { function, .. }) => {
            let args_preview = serde_json::from_str(&function.arguments)
                .ok()
                .as_ref()
                .map(format_args_preview)
                .unwrap_or_else(|| truncate_str(&function.arguments, 80));
            format!("\n> 🔧 **{}** `{}`\n", function.name, args_preview)
        }
        (Role::Tool, MessageContent::ToolResult { tool_call_id, content }) => {
            let args_preview = tool_args_cache
                .get(tool_call_id)
                .cloned()
                .unwrap_or_else(|| truncate_str(tool_call_id, 40));
            let output_block = format_output_block(content);
            format!("> ✅ **tool** `{}`{}\n", args_preview, output_block)
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
