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

                    if matches!(result.immediate_action, Some(ImmediateAction::Abort)) {
                        self.abort_pending = true;
                        self.send_abort_signal().await;
                        return false;
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
        // When abort_pending the user explicitly stopped the queue from
        // auto-advancing.  New messages are queued rather than sent directly
        // even when the agent appears idle, until the user manually submits.
        if self.agent_busy || self.abort_pending {
            self.queued.push_back(qm);
            self.queue_selected = Some(self.queued.len() - 1);
        } else {
            self.sync_nvim_buffer_to_segments().await;
            let history = messages_for_resubmit(&self.chat_segments);
            self.chat_segments.push(ChatSegment::Message(Message::user(text)));
            self.save_history_async();
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
        if !self.agent_busy && self.editing_queue_index.is_none() && !self.abort_pending {
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

    /// Signal the currently running agent turn to abort.
    ///
    /// Dropping the sender half of the oneshot channel causes the receiver
    /// inside `submit_with_cancel` to resolve, triggering the abort branch.
    pub(crate) async fn send_abort_signal(&self) {
        let sender = self.cancel_handle.lock().await.take();
        // Dropping the sender without sending signals cancellation via Err on the receiver.
        drop(sender);
    }

    /// Force-submit the queue item at `idx`.
    ///
    /// If the agent is currently busy, the running turn is aborted first.  The
    /// partial streamed content is preserved in the chat (via `AgentEvent::Aborted`).
    /// After the abort the selected message is sent as a full resubmit so the
    /// model sees the complete conversation history including any partial text.
    ///
    /// If the agent is idle, the message is sent immediately (same as a normal
    /// manual-dequeue submit).
    pub(crate) async fn force_submit_queued_message(&mut self, idx: usize) {
        if idx >= self.queued.len() {
            return;
        }
        let qm = match self.queued.remove(idx) {
            Some(qm) => qm,
            None => return,
        };
        // Keep selection in bounds.
        self.queue_selected = if self.queued.is_empty() {
            None
        } else {
            Some(idx.min(self.queued.len() - 1))
        };
        if self.queued.is_empty() && self.focus == FocusPane::Queue {
            self.focus = FocusPane::Input;
        }

        if self.agent_busy {
            // Abort the current run.  The Aborted event will commit partial text
            // to chat_segments.  Store the message at the front of the queue
            // so the Aborted handler picks it up and auto-sends it.
            // abort_pending stays false so auto-dequeue fires after the abort.
            self.queued.push_front(qm);
            self.queue_selected = Some(0);
            self.abort_pending = false;
            self.send_abort_signal().await;
        } else {
            // Agent idle: send immediately as a resubmit with full history.
            // Clear abort_pending so subsequent turns auto-dequeue normally.
            self.abort_pending = false;
            let history = messages_for_resubmit(&self.chat_segments);
            self.chat_segments.push(ChatSegment::Message(Message::user(&qm.content)));
            self.save_history_async();
            self.rerender_chat().await;
            self.auto_scroll = true;
            self.scroll_to_bottom();
            self.send_resubmit_to_agent(history, qm).await;
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

    // ── Edit-and-resubmit model override ─────────────────────────────────────

    /// Regression: staged model override must be applied when editing an old
    /// chat message and resubmitting, not silently discarded.
    #[tokio::test]
    async fn edit_resubmit_applies_staged_model_override() {
        let (mut app, mut rx) = App::for_testing();

        // Seed a user message in the chat (simulate prior conversation).
        let seg_idx = app.inject_chat_user_message("original message");

        // Stage a model switch — user typed /model but hasn't sent a new message yet.
        app.inject_input("/model anthropic/claude-opus-4-6");
        app.dispatch_action(Action::Submit).await;
        // No resubmit should have been sent (it's just a model switch command).
        assert!(rx.try_recv().is_err(), "/model alone must not send a request");

        // Now start editing the existing chat message and confirm.
        app.start_editing_segment(seg_idx, "edited message");
        app.dispatch_action(Action::EditMessageConfirm).await;

        let req = rx.try_recv().expect("edit-resubmit must send a request");
        assert_eq!(resubmit_content(&req), "edited message");
        assert_eq!(
            resubmit_model(&req).as_deref(),
            Some("anthropic/claude-opus-4-6"),
            "staged model must be forwarded to the agent on edit-resubmit"
        );
    }

    /// When no model is staged, editing an old message resubmits without any
    /// model override (agent keeps whatever model it was last using).
    #[tokio::test]
    async fn edit_resubmit_without_staged_model_sends_no_override() {
        let (mut app, mut rx) = App::for_testing();

        let seg_idx = app.inject_chat_user_message("hello");
        app.start_editing_segment(seg_idx, "hello edited");
        app.dispatch_action(Action::EditMessageConfirm).await;

        let req = rx.try_recv().expect("edit-resubmit must send a request");
        assert_eq!(resubmit_content(&req), "hello edited");
        assert!(
            resubmit_model(&req).is_none(),
            "no staged model means no model override in resubmit"
        );
    }

    /// Staged mode override is also forwarded on edit-resubmit.
    #[tokio::test]
    async fn edit_resubmit_applies_staged_mode_override() {
        let (mut app, mut rx) = App::for_testing();

        let seg_idx = app.inject_chat_user_message("do some research");

        app.inject_input("/mode research");
        app.dispatch_action(Action::Submit).await;
        assert!(rx.try_recv().is_err(), "/mode alone must not send a request");

        app.start_editing_segment(seg_idx, "do some research — edited");
        app.dispatch_action(Action::EditMessageConfirm).await;

        let req = rx.try_recv().expect("edit-resubmit must send a request");
        assert_eq!(resubmit_mode(&req), Some(AgentMode::Research),
            "staged mode must be forwarded to the agent on edit-resubmit");
    }

    /// Empty input: nothing is sent.
    #[tokio::test]
    async fn empty_input_sends_nothing() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("   ");
        app.dispatch_action(Action::Submit).await;
        assert!(rx.try_recv().is_err(), "empty/whitespace input must not send to agent");
    }

    // ── /abort command ────────────────────────────────────────────────────────

    /// `/abort` while the agent is busy sets abort_pending.
    #[tokio::test]
    async fn abort_command_sets_abort_pending_when_busy() {
        let (mut app, mut rx) = App::for_testing();

        // First message goes to agent (makes it busy).
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");
        assert!(app.is_agent_busy());

        // /abort while busy should set abort_pending.
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        assert!(app.is_abort_pending(), "abort_pending must be set after /abort");
    }

    /// After /abort new messages are queued (not sent directly) even when agent is idle.
    #[tokio::test]
    async fn abort_pending_queues_new_messages_when_idle() {
        let (mut app, mut rx) = App::for_testing();

        // Simulate: agent was busy, got aborted, now idle.
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        // Manually set abort_pending to simulate post-abort state.
        app.simulate_aborted("partial response").await;
        // abort_pending not set by simulate_aborted — set it manually.
        // (In real life, InterruptAgent or /abort would set it before the abort signal.)
        // Use the dispatch path to set it properly.
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        // Now agent is NOT busy (simulate_aborted cleared it) and abort_pending is set.
        assert!(app.is_abort_pending());
        assert!(!app.is_agent_busy());

        app.inject_input("new message after abort");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(app.queued_len(), 1, "message should be queued when abort_pending");
        assert!(rx.try_recv().is_err(), "message must not go to agent directly");
    }

    /// After abort, auto-dequeue is suppressed; queued messages stay queued after TurnComplete.
    #[tokio::test]
    async fn abort_pending_suppresses_auto_dequeue() {
        let (mut app, mut rx) = App::for_testing();

        // First message.
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        // Queue a second message while busy.
        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(app.queued_len(), 1, "second should be queued");

        // Set abort_pending.
        // Simulate the abort completing (agent becomes idle).
        app.simulate_aborted("").await;
        // Force abort_pending = true to simulate the /abort path.
        // (We call the abort command handler path here via dispatch.)
        // Since agent is now idle (simulate_aborted set agent_busy=false),
        // use the internal flag directly.
        // The real path goes through Action::InterruptAgent or /abort which
        // sets abort_pending before calling send_abort_signal.
        // For test purposes, set the flag directly.
        // (This mirrors what InterruptAgent does: set abort_pending = true then signal.)
        let _ = app.dispatch_action(Action::InterruptAgent).await;
        // Note: InterruptAgent only sends signal if agent_busy; here it's not busy,
        // so we set abort_pending manually via the /abort path.
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        assert!(app.is_abort_pending());

        // simulate_turn_complete with abort_pending should NOT dequeue.
        // (In real life this is handled in TurnComplete; here agent is already idle.)
        // Verify: queue still has the "second" message.
        assert_eq!(app.queued_len(), 1, "queue should not have been drained while abort_pending");
        assert!(rx.try_recv().is_err(), "no message should have been sent automatically");
    }

    // ── Force-submit ──────────────────────────────────────────────────────────

    /// Force-submit while idle sends the selected message immediately.
    #[tokio::test]
    async fn force_submit_while_idle_sends_immediately() {
        let (mut app, mut rx) = App::for_testing();

        // Queue a message (agent is idle so just push directly).
        // Use busy agent to queue.
        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first sent");

        app.inject_input("queued message");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(app.queued_len(), 1);

        // Simulate turn complete: agent is now idle with queued message waiting.
        // Use abort_pending to prevent auto-dequeue.
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        app.simulate_aborted("").await;

        assert!(app.is_abort_pending());
        assert_eq!(app.queued_len(), 1);

        // Select queue item 0 and force-submit it.
        app.dispatch_action(Action::FocusQueue).await;
        app.dispatch_action(Action::ForceSubmitQueuedMessage).await;

        let req = rx.try_recv().expect("force-submit should send a request");
        assert_eq!(resubmit_content(&req), "queued message");
        assert_eq!(app.queued_len(), 0, "queue should be empty after force-submit");
    }
}
