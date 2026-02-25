// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Unified user-input submission path — documentation and integration tests.
//!
//! # Message Lifecycle
//!
//! The journey from "user presses Enter" to "agent receives a message with the
//! correct model configured" passes through the following steps:
//!
//! **Step 1** — `handle_term_event` / `Action::Submit`
//!   (`app.rs` → `dispatch()`)
//!   Takes the trimmed input buffer and calls `App::submit_user_input()`.
//!
//! **Step 2** — `App::submit_user_input()`
//!   (`app.rs`, section "Unified submit path")
//!   Dispatches slash commands via `dispatch_command()` (staging model/mode overrides
//!   into `SessionState`) or falls through to the plain-text path.
//!   For `Action::SubmitBufferToAgent`, slash commands are handled by
//!   `App::submit_nvim_command()` (immediate apply, no staging).
//!
//! **Step 3** — `App::enqueue_or_send_text()`
//!   (`app.rs`, section "Unified submit path")
//!   Calls `SessionState::consume_staged()` which promotes the staged model to
//!   `model_display` (status bar reflects switch immediately) and returns
//!   `(model_cfg, mode)` for the `QueuedMessage`.
//!
//! **Step 4** — `QueuedMessage` construction
//!   The staged model config is converted to `"{provider}/{name}"` string for
//!   the `model_override` field (interim format; Step 4 of the refactor will
//!   replace this with `Arc<dyn ModelProvider>`).
//!   If the agent is busy, the message is pushed to `App::queued`.
//!   If the agent is idle, `App::send_resubmit_to_agent()` is called directly.
//!
//! **Step 5** — `App::send_resubmit_to_agent()`
//!   (`app.rs`)
//!   Sends `AgentRequest::Resubmit { messages, new_user_content, model_override,
//!   mode_override }` to the background agent task via `agent_tx`.
//!
//! **Step 6** — `agent_task` loop
//!   (`agent.rs`)
//!   Receives `AgentRequest::Resubmit`.  Resolves `model_override` string to a
//!   `ModelConfig` then to a `Box<dyn ModelProvider>` and calls `agent.set_model()`.
//!   (Step 4 of the refactor moves this resolution to the TUI side.)
//!
//! **Step 7** — `agent.replace_history_and_submit()`
//!   (`sven-core/src/agent.rs`)
//!   Replaces the agent's conversation history, appends the new user message,
//!   and runs the agentic loop.
//!
//! # Implementation Location
//!
//! The `submit_user_input`, `enqueue_or_send_text`, `submit_nvim_command`,
//! `send_to_agent`, `send_resubmit_to_agent`, and `try_dequeue_next` methods
//! live in this module (migrated from `app/mod.rs` in Step 5 of the refactor).

use sven_model::Message;

use crate::{
    agent::AgentRequest,
    app::{App, FocusPane, ModelDirective, QueuedMessage},
    chat::segment::{messages_for_resubmit, ChatSegment},
    commands::{dispatch_command, CommandContext, ImmediateAction},
};

impl App {
    // ── Submit path ───────────────────────────────────────────────────────────

    /// Process user input text: dispatch slash commands or send as a message.
    ///
    /// # Message Lifecycle
    ///
    /// 1. `Action::Submit` calls this with the trimmed input buffer text.
    ///    `Action::SubmitBufferToAgent` calls it (via `submit_nvim_command`)
    ///    for slash commands only; plain nvim text goes through
    ///    `send_resubmit_to_agent` directly after segment replacement.
    /// 2. If `text` starts with `/`: dispatch via `dispatch_command()`.
    ///    - Quit → return `true` (terminate event loop).
    ///    - Model override → `SessionState::stage_model()` (also applies mode
    ///      immediately when the agent is idle so the status bar updates).
    ///    - `message_to_send` → replace `text`, fall through to step 3.
    ///    - No message → return `false` (command consumed, no send).
    ///    - Unknown command → return `false`.
    /// 3. Plain-text (or injected `message_to_send`) path:
    ///    a. `SessionState::consume_staged()` promotes the staged model to
    ///       `model_display` and returns `(model, mode)` for the message.
    ///    b. `QueuedMessage` constructed with those values.
    ///    c. Agent busy → push to queue.
    ///       Agent idle → snapshot history, append user segment, rerender,
    ///       send via `send_resubmit_to_agent()`.
    /// 4. `agent_task` receives `AgentRequest::Resubmit`.
    /// 5. `agent.set_model()` / `agent.set_mode()` / `agent.replace_history_and_submit()`.
    pub(crate) async fn submit_user_input(&mut self, text: &str) -> bool {
        if text.starts_with('/') {
            let ctx = CommandContext {
                config: self.config.clone(),
                current_model_provider: self.session.model_cfg.provider.clone(),
                current_model_name: self.session.model_cfg.name.clone(),
            };
            match dispatch_command(text, &self.command_registry, &ctx) {
                Some((_name, result)) => {
                    if matches!(result.immediate_action, Some(ImmediateAction::Quit)) {
                        return true;
                    }

                    if let Some(model_str) = result.model_override {
                        let resolved =
                            sven_model::resolve_model_from_config(&self.config, &model_str);
                        self.session.stage_model(resolved);
                    }

                    if let Some(mode) = result.mode_override {
                        self.session.stage_mode(mode);
                        // Apply immediately when idle so the status bar reflects
                        // the change before the next message is sent.
                        if !self.agent_busy {
                            self.session.mode = mode;
                        }
                    }

                    match result.message_to_send {
                        None => return false,
                        Some(msg) => {
                            return self.enqueue_or_send_text(&msg).await;
                        }
                    }
                }
                None => return false,
            }
        }

        self.enqueue_or_send_text(text).await
    }

    /// Consume staged overrides and either enqueue or send `text` to the agent.
    pub(crate) async fn enqueue_or_send_text(&mut self, text: &str) -> bool {
        self.auto_scroll = true;
        let (staged_model, staged_mode) = self.session.consume_staged();
        let qm = QueuedMessage {
            content: text.to_string(),
            model_transition: staged_model.map(ModelDirective::SwitchTo),
            mode_transition: staged_mode,
        };
        if self.agent_busy {
            self.queued.push_back(qm);
            self.queue_selected = Some(self.queued.len() - 1);
        } else {
            self.sync_nvim_buffer_to_segments().await;
            let history = messages_for_resubmit(&self.chat_segments);
            self.chat_segments.push(ChatSegment::Message(Message::user(text)));
            self.rerender_chat().await;
            self.scroll_to_bottom();
            self.send_resubmit_to_agent(history, qm).await;
        }
        false
    }


    /// Handle a slash command from the Neovim buffer (apply immediately, no staging).
    ///
    /// In the nvim-buffer path, the full conversation is already in the buffer,
    /// so there is no "next message" to attach overrides to.  Model and mode
    /// changes take effect right away.
    pub(crate) async fn submit_nvim_command(&mut self, text: &str) -> bool {
        let ctx = CommandContext {
            config: self.config.clone(),
            current_model_provider: self.session.model_cfg.provider.clone(),
            current_model_name: self.session.model_cfg.name.clone(),
        };
        if let Some((_name, result)) = dispatch_command(text, &self.command_registry, &ctx) {
            if matches!(result.immediate_action, Some(ImmediateAction::Quit)) {
                return true;
            }
            if let Some(model_str) = result.model_override {
                let resolved =
                    sven_model::resolve_model_from_config(&self.config, &model_str);
                self.session.apply_model(resolved);
            }
            if let Some(mode) = result.mode_override {
                self.session.apply_mode(mode);
            }
            // message_to_send is intentionally ignored for nvim-buffer commands:
            // the buffer already represents the full conversation state.
        }
        false
    }

    pub(crate) async fn send_to_agent(&mut self, qm: QueuedMessage) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Submit {
                    content: qm.content,
                    model_override: qm.model_transition.map(ModelDirective::into_model_config),
                    mode_override: qm.mode_transition,
                })
                .await;
            self.agent_busy = true;
        }
    }

    pub(crate) async fn send_resubmit_to_agent(
        &mut self,
        messages: Vec<Message>,
        qm: QueuedMessage,
    ) {
        if let Some(tx) = &self.agent_tx {
            let _ = tx
                .send(AgentRequest::Resubmit {
                    messages,
                    new_user_content: qm.content,
                    model_override: qm.model_transition.map(ModelDirective::into_model_config),
                    mode_override: qm.mode_transition,
                })
                .await;
            self.agent_busy = true;
        }
    }

    /// If the agent is currently idle and there are queued messages waiting,
    /// dequeue the first one and send it.  Called after a queue-item edit ends
    /// so that a turn that completed while the user was editing isn't dropped.
    pub(crate) async fn try_dequeue_next(&mut self) {
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
}

// ── Integration tests ─────────────────────────────────────────────────────────
//
// These tests exercise the full "user input → AgentRequest channel" path through
// the real App + SessionState machinery with a mock agent receiver.
// Every regression we found during the slash-command / model-switching work was
// at exactly this boundary; these tests provide the safety net that was missing.

#[cfg(test)]
mod submit_integration_tests {
    use sven_config::AgentMode;

    use crate::agent::AgentRequest;
    use crate::app::App;
    use crate::keys::Action;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Extract the `new_user_content` from a `Resubmit` request.
    fn resubmit_content(req: &AgentRequest) -> &str {
        match req {
            AgentRequest::Resubmit { new_user_content, .. } => new_user_content,
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    /// Extract the `model_override` display label from a `Resubmit` request.
    fn resubmit_model(req: &AgentRequest) -> Option<String> {
        match req {
            AgentRequest::Resubmit { model_override, .. } => {
                model_override.as_ref().map(|c| format!("{}/{}", c.provider, c.name))
            }
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    /// Extract the `mode_override` from a `Resubmit` request.
    fn resubmit_mode(req: &AgentRequest) -> Option<AgentMode> {
        match req {
            AgentRequest::Resubmit { mode_override, .. } => *mode_override,
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    /// Check that `queued_message_retains_model_override` still works after the rename.
    /// `model_display()` reflects what was consumed into `session.model_display`.
    fn expected_model_display_after_queue(app: &crate::app::App) -> &str {
        app.model_display()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Plain message: no overrides — content reaches agent unchanged.
    #[tokio::test]
    async fn plain_message_reaches_agent() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("hello world");
        app.dispatch_action(Action::Submit).await;

        let req = rx.try_recv().expect("expected a request");
        assert_eq!(resubmit_content(&req), "hello world");
        assert!(resubmit_model(&req).is_none(), "no model override expected");
        assert!(resubmit_mode(&req).is_none());
    }

    /// `/model openai/gpt-4o` followed by a message → agent receives the model
    /// override and the correct message content.
    #[tokio::test]
    async fn model_command_then_message_sends_correct_model() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("/model openai/gpt-4o");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("hello");
        app.dispatch_action(Action::Submit).await;

        let req = rx.try_recv().expect("expected a request");
        assert_eq!(resubmit_content(&req), "hello");
        assert_eq!(resubmit_model(&req).as_deref(), Some("openai/gpt-4o"));
    }

    /// After the model override is consumed, subsequent messages carry no override.
    #[tokio::test]
    async fn model_override_consumed_after_first_message() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("/model openai/gpt-4o");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;

        let first = rx.try_recv().expect("first request");
        assert_eq!(resubmit_model(&first).as_deref(), Some("openai/gpt-4o"));

        // Simulate turn complete so the second message is sent directly.
        app.simulate_turn_complete();

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        let second = rx.try_recv().expect("second request");
        assert!(
            resubmit_model(&second).is_none(),
            "model override must not persist to second message"
        );
    }

    /// `/mode research` followed by a message → agent receives the mode override.
    #[tokio::test]
    async fn mode_command_then_message_sends_correct_mode() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("/mode research");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("hello");
        app.dispatch_action(Action::Submit).await;

        let req = rx.try_recv().expect("expected a request");
        assert_eq!(resubmit_mode(&req), Some(AgentMode::Research));
    }

    /// `/quit` returns `true` (event-loop termination signal).
    #[tokio::test]
    async fn quit_command_returns_true() {
        let (mut app, _rx) = App::for_testing();
        app.inject_input("/quit");
        let quit = app.dispatch_action(Action::Submit).await;
        assert!(quit, "/quit must return true to terminate the event loop");
    }

    /// Unknown slash command: no message sent, returns false.
    #[tokio::test]
    async fn unknown_command_sends_nothing() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("/doesnotexist foo");
        let quit = app.dispatch_action(Action::Submit).await;
        assert!(!quit);
        assert!(rx.try_recv().is_err(), "unknown command must not send to agent");
    }

    /// When the agent is busy, messages are queued instead of sent.
    #[tokio::test]
    async fn busy_agent_queues_messages() {
        let (mut app, mut rx) = App::for_testing();

        // First message: goes through because agent is idle.
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        // Simulate agent becoming busy (normally set when a request is sent).
        // After the first Resubmit the agent_tx side marks busy; we replicate
        // that here by sending another message while the app still thinks it's
        // idle, then checking the queue for a third message sent while busy.
        // Note: send_resubmit_to_agent sets agent_busy = true.
        assert!(app.is_agent_busy(), "agent should be busy after first send");

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        assert_eq!(app.queued_len(), 1, "second message should be queued while agent busy");
        assert!(rx.try_recv().is_err(), "no second request should reach agent yet");
    }

    /// Queued message with a staged model retains the override when dequeued.
    #[tokio::test]
    async fn queued_message_retains_model_override() {
        let (mut app, mut rx) = App::for_testing();

        // First message goes to agent.
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message");

        // Agent busy; stage model then send second message (goes to queue).
        app.inject_input("/model anthropic/claude-opus-4-6");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        assert_eq!(app.queued_len(), 1);
        // The queued message should carry the staged model override.
        // We can't observe it directly here without exposing the queue,
        // but model_display is updated on consume_staged; verify it changed.
        assert_eq!(
            app.model_display(),
            "anthropic/claude-opus-4-6",
            "model_display should be promoted when override is consumed into queue"
        );
    }

    /// Empty input: nothing is sent.
    #[tokio::test]
    async fn empty_input_sends_nothing() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("   ");
        app.dispatch_action(Action::Submit).await;
        assert!(rx.try_recv().is_err(), "empty/whitespace input must not send to agent");
    }
}
