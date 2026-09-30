// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The TUI's top-level async event loop: wires up the agent task, the
//! embedded Neovim process (if enabled), and the `tokio::select!` that
//! multiplexes agent events, terminal events, questions, toasts, and the
//! animation tick into calls on `App`'s other methods.

use std::sync::Arc;

use crossterm::event::EventStream;
use futures::StreamExt;
use ratatui::{layout::Rect, DefaultTerminal};
use sven_machines::AgentEvent;
use sven_mcp_client::{McpEvent, McpManager};
use sven_model::Message;
use sven_tools_agent::QuestionRequest;
use sven_tui_nvim::NvimBridge;
use tokio::sync::mpsc;

use crate::{
    agent::{kernel_session_task, AgentRequest},
    app::{ui_state, App, FocusPane},
    chat::segment::ChatSegment,
    keys::Action,
    layout::AppLayout,
    node_agent::node_agent_task,
    overlay::question::{watch_withdrawal, QuestionModal},
};

impl App {
    /// Run the TUI event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        let (submit_tx, submit_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        let (question_tx, mut question_rx) = mpsc::channel::<sven_tools_agent::QuestionRequest>(4);
        let (toast_tx, mut toast_rx) = mpsc::channel::<ui_state::Toast>(32);
        let (withdrawn_tx, mut withdrawn_rx) = mpsc::channel::<String>(8);
        self.question_withdrawn_tx = Some(withdrawn_tx);
        self.toast_tx = Some(toast_tx);

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
        // Remove per-agent event_rx - events now flow through the mux channel.
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
            let (mcp_refresh_tx, _) = tokio::sync::broadcast::channel(16);
            self.mcp_refresh_tx = Some(mcp_refresh_tx.clone());

            let cfg = self.config.clone();
            let mode = self.session.mode;
            let startup_model_cfg = self.session.model_cfg.clone();
            let cancel_handle_task = self.agent.cancel.clone();
            let shared_skills_task = self.shared_skills.clone();
            let shared_agents_task = self.shared_agents.clone();
            let (mcp_tx, mcp_rx) = tokio::sync::oneshot::channel::<(
                Arc<McpManager>,
                tokio::sync::mpsc::Receiver<sven_mcp_client::McpEvent>,
            )>();
            tokio::spawn(async move {
                kernel_session_task(
                    cfg,
                    startup_model_cfg,
                    mode,
                    submit_rx,
                    event_tx,
                    question_tx,
                    cancel_handle_task,
                    shared_skills_task,
                    shared_agents_task,
                    Some(mcp_tx),
                )
                .await;
            });
            // Receive the McpManager and event receiver from the agent task.
            if let Ok((mgr, mcp_event_rx)) = mcp_rx.await {
                let prompt_commands = crate::commands::mcp::discover_mcp_prompts(&mgr).await;
                for cmd in prompt_commands {
                    let cmd: Arc<dyn crate::commands::SlashCommand> = Arc::new(cmd);
                    self.mcp_prompt_commands.insert(cmd.name().to_string(), cmd);
                }
                self.mcp_manager = Some(Arc::clone(&mgr));
                // Spawn a background task to forward MCP events as TUI toasts.
                if let Some(ref tx) = self.toast_tx {
                    let toast_tx = tx.clone();
                    let mgr_clone = Arc::clone(&mgr);
                    tokio::spawn(async move {
                        mcp_event_consumer(mcp_event_rx, toast_tx, mcp_refresh_tx, mgr_clone).await;
                    });
                }
            }
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

        // Enable bracketed paste. Mouse capture and the Kitty
        // keyboard-enhancement flags are set up exactly once, in
        // `run_tui` (`src/main.rs`), gated on
        // `supports_keyboard_enhancement()`. Pushing the keyboard flags a
        // second time here used to create an unbalanced Push/Pop pair
        // against `run_tui`'s single Pop on exit, which could leave a
        // terminal that doesn't fully support the protocol relying on the
        // raw escape-timeout heuristic to disambiguate `ESC` from the start
        // of an arrow-key sequence - and could leave the terminal stuck in
        // enhanced-keyboard mode after an unclean exit.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);

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
                Some((session_id, agent_event)) = self.recv_agent_event() => {
                    if self.handle_agent_event(session_id, agent_event).await { break; }
                }
                Some(Ok(term_event)) = crossterm_events.next() => {
                    if self.handle_term_event(term_event).await { break; }
                }
                Some(req) = question_rx.recv() => {
                    self.handle_question_request(req);
                }
                Some(id) = withdrawn_rx.recv() => {
                    self.withdraw_question(&id);
                }
                Some(toast) = toast_rx.recv() => {
                    self.ui.push_toast(toast);
                }
                // Idle-must-be-stable invariant: this is the ONLY branch that can
                // fire on a timer, and it is gated on a busy flag. When nothing is
                // busy the `select!` blocks on real events, so `terminal.draw()`
                // runs only when state actually changes and (thanks to ratatui's
                // cell diff) emits nothing for a stable view — no cursor flicker,
                // and the user can select/copy terminal text. It follows that
                // every path which sets a busy flag MUST have a path that clears
                // it (see node_agent's turn-exit guard for the remote case);
                // a stuck busy flag would repaint the screen at 12fps forever.
                _ = anim_tick.tick(), if self.agent.busy || self.sessions.any_background_busy() => {
                    // Advance the clock-driven animation frame. This branch also
                    // serves as the display refresh for streaming text: TextDelta
                    // handlers only buffer incoming tokens; the actual render
                    // (build_display_from_segments + Neovim sync) is rate-limited
                    // here to 80ms so fast LLM streams don't starve keyboard events.
                    self.agent.anim_frame = self.agent.anim_frame.wrapping_add(1);
                    self.rerender_chat().await;
                    self.scroll_to_bottom();
                    self.nvim_scroll_to_bottom().await;
                    if let Some(pager) = &mut self.ui.pager {
                        pager.set_lines(self.chat.lines.clone());
                    }
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

    /// Shows a question from the agent, watching for its withdrawal.
    pub(crate) fn handle_question_request(&mut self, req: QuestionRequest) {
        tracing::debug!(id = %req.id, count = req.questions.len(), "question request received");
        let answer_tx = match &self.question_withdrawn_tx {
            Some(withdrawn) => watch_withdrawal(req.id.clone(), req.answer_tx, withdrawn.clone()),
            None => req.answer_tx,
        };
        self.ui.question_modal = Some(QuestionModal::new(req.id, req.questions, answer_tx));
        self.ui.focus = FocusPane::Input;
    }

    /// Takes down the question `id` if it is the one on screen: whoever
    /// asked it no longer wants the answer.
    pub(crate) fn withdraw_question(&mut self, id: &str) {
        if self
            .ui
            .question_modal
            .as_ref()
            .is_some_and(|m| m.id() == id)
        {
            self.ui.question_modal = None;
        }
    }

    pub(crate) async fn recv_agent_event(
        &mut self,
    ) -> Option<(sven_session_store::SessionId, AgentEvent)> {
        self.sessions.multi_event_rx.recv().await
    }

    async fn nvim_notify_future(notify: Option<&tokio::sync::Notify>) {
        match notify {
            Some(n) => n.notified().await,
            None => std::future::pending().await,
        }
    }
}

// ── MCP event consumer ────────────────────────────────────────────────────────

/// Background task that consumes `McpEvent`s and forwards them as TUI toasts.
/// When `ToolsChanged` fires, sends on `refresh_tx` so all agent tasks refresh
/// their MCP tool registries.
async fn mcp_event_consumer(
    mut rx: mpsc::Receiver<McpEvent>,
    toast_tx: mpsc::Sender<ui_state::Toast>,
    refresh_tx: tokio::sync::broadcast::Sender<()>,
    _mgr: Arc<McpManager>,
) {
    while let Some(event) = rx.recv().await {
        if matches!(event, McpEvent::ToolsChanged) {
            let _ = refresh_tx.send(());
        }
        let toast = match event {
            McpEvent::AuthStarted { ref server } => Some(ui_state::Toast::info(format!(
                "Opening browser to authenticate with '{server}'..."
            ))),
            McpEvent::AuthRequired { ref server, .. } => Some(ui_state::Toast::warning(format!(
                "'{server}' requires authentication - run `/mcp auth {server}`"
            ))),
            McpEvent::ServerConnected(ref server) => Some(ui_state::Toast::success(format!(
                "MCP server '{server}' connected"
            ))),
            McpEvent::ServerFailed {
                ref name,
                ref error,
            } => {
                // Only surface non-auth failures as toasts (auth failures
                // are already shown via AuthStarted/AuthRequired).
                if error.contains("401") || error.contains("Unauthorized") {
                    None
                } else {
                    // Show a generous slice of the full error chain so the
                    // user sees the actual HTTP response, not just the context
                    // wrapper (e.g. "HTTP 403: ..." rather than "MCP initialize").
                    let summary = error.chars().take(300).collect::<String>();
                    Some(ui_state::Toast::error(format!("MCP '{name}': {summary}")))
                }
            }
            _ => None,
        };
        if let Some(t) = toast {
            let _ = toast_tx.send(t).await;
        }
    }
}
