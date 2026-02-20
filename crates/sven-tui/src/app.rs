use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tracing::debug;

use ratatui::layout::Rect;
use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent};
use sven_tools::{FsTool, GlobTool, ShellTool, ToolRegistry};

use crate::{
    keys::{map_key, Action},
    layout::AppLayout,
    markdown::{render_markdown, StyledLines},
    widgets::{draw_chat, draw_help, draw_input, draw_search, draw_status},
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

/// Search state.
#[derive(Debug, Default)]
struct SearchState {
    active: bool,
    query: String,
    matches: Vec<usize>,
    current: usize,
}

impl SearchState {
    fn update_matches(&mut self, lines: &StyledLines) {
        if self.query.is_empty() {
            self.matches.clear();
            return;
        }
        let q = self.query.to_lowercase();
        self.matches = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.spans.iter().any(|s| s.content.to_lowercase().contains(&q))
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

/// The top-level TUI application state.
pub struct App {
    config: Arc<Config>,
    mode: AgentMode,
    focus: FocusPane,
    chat_lines: StyledLines,
    /// Raw markdown buffer (used for re-render on resize)
    chat_raw: String,
    scroll_offset: u16,
    input_buffer: String,
    input_cursor: usize,
    queued: VecDeque<String>,
    search: SearchState,
    show_help: bool,
    agent_busy: bool,
    context_pct: u8,
    agent_tx: Option<mpsc::Sender<String>>,
    event_rx: Option<mpsc::Receiver<AgentEvent>>,
    /// True after Ctrl+w is pressed, waiting for j/k to complete the chord.
    pending_nav: bool,
    /// Visible inner height of the chat pane (rows inside the border).
    /// Updated every frame so scroll calculations are correct.
    chat_height: u16,
}

impl App {
    pub fn new(config: Arc<Config>, opts: AppOptions) -> Self {
        let mut app = Self {
            config,
            mode: opts.mode,
            focus: FocusPane::Input,
            chat_lines: Vec::new(),
            chat_raw: String::new(),
            scroll_offset: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            queued: VecDeque::new(),
            search: SearchState::default(),
            show_help: false,
            agent_busy: false,
            context_pct: 0,
            agent_tx: None,
            event_rx: None,
            pending_nav: false,
            chat_height: 24, // sensible default; updated every frame
        };

        if let Some(prompt) = opts.initial_prompt {
            app.queued.push_back(prompt);
        }

        app
    }

    /// Run the TUI event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        // Spawn the agent in a background task
        let (submit_tx, submit_rx) = mpsc::channel::<String>(64);
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        self.agent_tx = Some(submit_tx);
        self.event_rx = Some(event_rx);

        let cfg = self.config.clone();
        let mode = self.mode;

        tokio::spawn(async move {
            agent_task(cfg, mode, submit_rx, event_tx).await;
        });

        // Send any queued initial prompt
        if let Some(p) = self.queued.pop_front() {
            self.send_to_agent(p).await;
        }

        let mut crossterm_events = EventStream::new();

        loop {
            // Compute layout from terminal size so scroll helpers have the
            // correct visible height before the draw closure runs.
            if let Ok(size) = terminal.size() {
                let layout = AppLayout::compute(
                    Rect::new(0, 0, size.width, size.height),
                    self.search.active,
                );
                self.chat_height = layout.chat_inner_height().max(1);
            }

            let ascii = self.ascii();
            terminal.draw(|frame| {
                let layout = AppLayout::new(frame, self.search.active);
                let model_name = &self.config.model.name;

                draw_status(
                    frame,
                    layout.status_bar,
                    model_name,
                    self.mode,
                    self.context_pct,
                    self.agent_busy,
                    ascii,
                );
                draw_chat(
                    frame,
                    layout.chat_pane,
                    &self.chat_lines,
                    self.scroll_offset,
                    self.focus == FocusPane::Chat,
                    ascii,
                );
                draw_input(
                    frame,
                    layout.input_pane,
                    &self.input_buffer,
                    self.input_cursor,
                    self.focus == FocusPane::Input,
                    self.queued.len(),
                    ascii,
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
            })?;

            // Poll agent events and crossterm events concurrently
            tokio::select! {
                Some(agent_event) = self.recv_agent_event() => {
                    if self.handle_agent_event(agent_event) {
                        break;
                    }
                }
                Some(Ok(term_event)) = crossterm_events.next() => {
                    if self.handle_term_event(term_event).await {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn recv_agent_event(&mut self) -> Option<AgentEvent> {
        if let Some(rx) = &mut self.event_rx {
            rx.recv().await
        } else {
            None
        }
    }

    /// Returns true when the app should quit.
    fn handle_agent_event(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::TextDelta(delta) => {
                self.chat_raw.push_str(&delta);
                self.rerender_chat();
                self.scroll_to_bottom();
            }
            AgentEvent::ToolCallStarted(tc) => {
                let line = format!("\n> **tool** `{}` …\n", tc.name);
                self.chat_raw.push_str(&line);
                self.rerender_chat();
                self.scroll_to_bottom();
            }
            AgentEvent::ToolCallFinished { tool_name, is_error, output, .. } => {
                let prefix = if is_error { "⚠ " } else { "✓ " };
                let preview = output.lines().next().unwrap_or("").chars().take(80).collect::<String>();
                let line = format!("\n> {prefix}**{tool_name}**: {preview}\n\n");
                self.chat_raw.push_str(&line);
                self.rerender_chat();
                self.scroll_to_bottom();
            }
            AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
                let note = format!("\n---\n*Context compacted: {} → {} tokens*\n\n", tokens_before, tokens_after);
                self.chat_raw.push_str(&note);
                self.rerender_chat();
            }
            AgentEvent::TokenUsage { input, output, context_total: _ } => {
                let max = 128_000u32;
                self.context_pct = ((input + output) * 100 / max.max(1)).min(100) as u8;
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                // Send next queued message if any
                if let Some(next) = self.queued.pop_front() {
                    let tx = self.agent_tx.clone().unwrap();
                    tokio::spawn(async move { let _ = tx.send(next).await; });
                    self.agent_busy = true;
                }
            }
            AgentEvent::Error(msg) => {
                let line = format!("\n**Error**: {msg}\n\n");
                self.chat_raw.push_str(&line);
                self.rerender_chat();
                self.agent_busy = false;
            }
            _ => {}
        }
        false
    }

    /// Returns true when the app should quit.
    async fn handle_term_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                // Dismiss help overlay on any keypress
                if self.show_help {
                    self.show_help = false;
                    return false;
                }

                let in_search = self.search.active;
                let in_input  = self.focus == FocusPane::Input;

                if let Some(action) = map_key(k, in_search, in_input, self.pending_nav) {
                    // NavPrefix sets the pending flag; all other actions clear it
                    if action == Action::NavPrefix {
                        self.pending_nav = true;
                        return false;
                    }
                    self.pending_nav = false;
                    return self.dispatch(action).await;
                }
                // No action matched — if we were waiting for a nav chord, cancel it
                self.pending_nav = false;
                false
            }

            // Mouse wheel scrolling in the chat pane
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp   => self.scroll_up(3),
                    MouseEventKind::ScrollDown => self.scroll_down(3),
                    _ => {}
                }
                false
            }

            Event::Resize(_, _) => {
                self.rerender_chat();
                false
            }

            _ => false,
        }
    }

    /// Dispatch an [`Action`]. Returns true to quit.
    #[allow(unreachable_patterns)]
    async fn dispatch(&mut self, action: Action) -> bool {
        match action {
            Action::Quit => return true,

            // Pane focus
            Action::FocusChat => self.focus = FocusPane::Chat,
            Action::FocusInput => self.focus = FocusPane::Input,

            // Scrolling
            Action::ScrollUp => self.scroll_up(1),
            Action::ScrollDown => self.scroll_down(1),
            Action::ScrollPageUp => self.scroll_up(20),
            Action::ScrollPageDown => self.scroll_down(20),
            Action::ScrollTop => self.scroll_offset = 0,
            Action::ScrollBottom => self.scroll_to_bottom(),

            // Search
            Action::SearchOpen => {
                self.search.active = true;
                self.focus = FocusPane::Chat;
            }
            Action::SearchClose => {
                self.search.active = false;
            }
            Action::SearchInput(c) => {
                self.search.query.push(c);
                self.search.update_matches(&self.chat_lines);
                if let Some(line) = self.search.current_line() {
                    self.scroll_offset = line as u16;
                }
            }
            Action::SearchBackspace => {
                self.search.query.pop();
                self.search.update_matches(&self.chat_lines);
            }
            Action::SearchNextMatch => {
                if !self.search.matches.is_empty() {
                    self.search.current = (self.search.current + 1) % self.search.matches.len();
                    if let Some(line) = self.search.current_line() {
                        self.scroll_offset = line as u16;
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
                    }
                }
            }

            // Input editing
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
                    let prev = self.prev_char_boundary(self.input_cursor);
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
                self.input_cursor = self.prev_char_boundary(self.input_cursor);
            }
            Action::InputMoveCursorRight => {
                if self.input_cursor < self.input_buffer.len() {
                    let ch = self.input_buffer[self.input_cursor..]
                        .chars().next().map(|c| c.len_utf8()).unwrap_or(1);
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
            Action::InputMoveLineEnd => self.input_cursor = self.input_buffer.len(),
            Action::InputDeleteToEnd => {
                self.input_buffer.truncate(self.input_cursor);
            }
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
                        // Echo user message into chat
                        self.chat_raw.push_str(&format!("\n**You:** {text}\n\n"));
                        self.rerender_chat();
                        self.scroll_to_bottom();
                        self.send_to_agent(text).await;
                    }
                }
            }

            Action::InterruptAgent => {
                // TODO: send cancellation signal to agent
            }

            Action::CycleMode => {
                self.mode = match self.mode {
                    AgentMode::Research => AgentMode::Plan,
                    AgentMode::Plan => AgentMode::Agent,
                    AgentMode::Agent => AgentMode::Research,
                };
            }

            Action::Help => {
                self.show_help = !self.show_help;
            }

            _ => {}
        }
        false
    }

    async fn send_to_agent(&mut self, text: String) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx.send(text).await;
            self.agent_busy = true;
        }
    }

    fn rerender_chat(&mut self) {
        self.chat_lines = render_markdown(&self.chat_raw, self.config.tui.wrap_width, self.ascii());
        self.search.update_matches(&self.chat_lines);
    }

    /// Whether to use ASCII-only borders/indicators instead of Unicode glyphs.
    /// Controlled by `tui.ascii_borders` in config or the `SVEN_ASCII_BORDERS`
    /// environment variable.
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
        self.scroll_offset = (self.chat_lines.len() as u16).saturating_sub(self.chat_height);
    }

    fn prev_char_boundary(&self, pos: usize) -> usize {
        if pos == 0 { return 0; }
        let mut p = pos - 1;
        while p > 0 && !self.input_buffer.is_char_boundary(p) {
            p -= 1;
        }
        p
    }
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

/// Background task that owns the [`Agent`] and forwards events.
async fn agent_task(
    config: Arc<Config>,
    mode: AgentMode,
    mut rx: mpsc::Receiver<String>,
    tx: mpsc::Sender<AgentEvent>,
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

    let mut agent = Agent::new(
        model,
        Arc::new(registry),
        Arc::new(config.agent.clone()),
        mode,
        128_000,
    );

    while let Some(msg) = rx.recv().await {
        debug!(msg_len = msg.len(), "agent task received message");
        if let Err(e) = agent.submit(&msg, tx.clone()).await {
            let _ = tx.send(AgentEvent::Error(e.to_string())).await;
        }
    }
}
