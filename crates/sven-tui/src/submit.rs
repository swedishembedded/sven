// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
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
//!   Dispatches slash commands via `dispatch_command()` (staging model/mode overrides
//!   into `SessionState`) or falls through to the plain-text path.
//!   For `Action::SubmitBufferToAgent`, slash commands are handled by
//!   `App::submit_nvim_command()` (immediate apply, no staging).
//!
//! **Step 3** — `App::enqueue_or_send_text()`
//!   Calls `SessionState::consume_staged()` which promotes the staged model to
//!   `model_display` (status bar reflects switch immediately) and returns
//!   `(model_cfg, mode)` for the `QueuedMessage`.
//!
//! **Step 4** — `QueuedMessage` construction
//!   The staged model config is converted to `"{provider}/{name}"` string for
//!   the `model_override` field.
//!   If the agent is busy, the message is pushed to `App::queue.messages`.
//!   If the agent is idle, `App::send_resubmit_to_agent()` is called directly.
//!
//! **Step 5** — `App::send_resubmit_to_agent()`
//!   Sends `AgentRequest::Resubmit { messages, new_user_content, model_override,
//!   mode_override }` to the background agent task via `agent.tx`.
//!
//! **Step 6** — `agent_task` loop
//!   Receives `AgentRequest::Resubmit`.  Resolves `model_override` string to a
//!   `ModelConfig` then to a `Box<dyn ModelProvider>` and calls `agent.set_model()`.
//!
//! **Step 7** — `agent.replace_history_and_submit()`
//!   Replaces the agent's conversation history, appends the new user message,
//!   and runs the agentic loop.

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
                        self.queue.abort_pending = true;
                        self.send_abort_signal().await;
                        return false;
                    }

                    if matches!(result.immediate_action, Some(ImmediateAction::ClearChat)) {
                        self.chat.segments.clear();
                        self.chat.tool_args.clear();
                        self.save_history_async();
                        self.rerender_chat().await;
                        return false;
                    }

                    if matches!(
                        result.immediate_action,
                        Some(ImmediateAction::NewConversation)
                    ) {
                        self.new_session().await;
                        return false;
                    }

                    if matches!(
                        result.immediate_action,
                        Some(ImmediateAction::RefreshSkills)
                    ) {
                        // Skills are rescanned lazily; a simple toast acknowledgement is sufficient.
                        self.ui
                            .push_toast(crate::app::ui_state::Toast::info("Skills refreshed"));
                        return false;
                    }

                    if matches!(
                        result.immediate_action,
                        Some(ImmediateAction::OpenTeamPicker)
                    ) {
                        self.ui.show_help = false;
                        self.ui.toggle_team_picker();
                        return false;
                    }

                    if matches!(
                        result.immediate_action,
                        Some(ImmediateAction::ToggleTaskList)
                    ) {
                        use crate::pager::PagerOverlay;
                        if self.ui.pager.is_none() {
                            let placeholder = "Task list is not available in this session.\n\
                                               Connect to a team-enabled sven node to see tasks.";
                            use crate::markdown::StyledLines;
                            let lines =
                                StyledLines::from(vec![ratatui::text::Line::from(placeholder)]);
                            self.ui.pager = Some(PagerOverlay::new(lines));
                        } else {
                            self.ui.pager = None;
                        }
                        return false;
                    }

                    if let Some(ImmediateAction::OpenInspector { ref kind }) =
                        result.immediate_action
                    {
                        use crate::ui::{InspectorKind, InspectorOverlay};
                        let ascii = self.ascii();
                        let skills = self.shared_skills.get();
                        let agents = self.shared_agents.get();
                        let buffer_store = std::sync::Arc::clone(&self.buffer_store);
                        let project_root = sven_runtime::find_project_root().ok();
                        let is_node = self.is_node_proxy;
                        let inspector = match kind {
                            InspectorKind::Skills => {
                                InspectorOverlay::for_skills(&skills, is_node, ascii)
                            }
                            InspectorKind::Subagents => {
                                InspectorOverlay::for_subagents(&agents, is_node, ascii)
                            }
                            InspectorKind::Peers => InspectorOverlay::for_peers(
                                &agents,
                                Some(buffer_store),
                                is_node,
                                ascii,
                            ),
                            InspectorKind::Context => InspectorOverlay::for_context(
                                project_root.as_deref(),
                                Some(buffer_store),
                                is_node,
                                ascii,
                            ),
                            InspectorKind::Tools => {
                                let tools = if is_node {
                                    // Fetch live from the node.
                                    let url = self.node_url.clone().unwrap_or_default();
                                    let token = self.node_token.clone().unwrap_or_default();
                                    let insecure = self.node_insecure;
                                    crate::node_agent::fetch_node_tools(&url, &token, insecure)
                                        .await
                                } else {
                                    self.shared_tools.get().to_vec()
                                };
                                InspectorOverlay::for_tools(&tools, is_node, ascii)
                            }
                        };
                        self.ui.inspector = Some(inspector);
                        return false;
                    }

                    if let Some(ImmediateAction::ApprovePlan { ref task_id }) =
                        result.immediate_action
                    {
                        let ev = sven_core::CollabEvent::PlanApproved {
                            name: String::new(),
                            task_id: task_id.clone(),
                        };
                        self.chat
                            .segments
                            .push(crate::chat::segment::ChatSegment::CollabEvent(ev));
                        self.save_history_async();
                        self.rerender_chat().await;
                        return false;
                    }

                    if let Some(ImmediateAction::RejectPlan {
                        ref task_id,
                        ref feedback,
                    }) = result.immediate_action
                    {
                        let ev = sven_core::CollabEvent::PlanRejected {
                            name: String::new(),
                            task_id: task_id.clone(),
                            feedback: feedback.clone(),
                        };
                        self.chat
                            .segments
                            .push(crate::chat::segment::ChatSegment::CollabEvent(ev));
                        self.save_history_async();
                        self.rerender_chat().await;
                        return false;
                    }

                    // In node-proxy mode the node owns model/mode selection;
                    // silently ignore /model and /mode commands.
                    if !self.is_node_proxy {
                        if let Some(model_str) = result.model_override {
                            let resolved =
                                sven_model::resolve_model_from_config(&self.config, &model_str);
                            self.session.stage_model(resolved);
                        }

                        if let Some(mode) = result.mode_override {
                            self.session.stage_mode(mode);
                            if !self.agent.busy {
                                self.session.mode = mode;
                            }
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
        self.chat.auto_scroll = true;
        let (staged_model, staged_mode) = self.session.consume_staged();
        let qm = QueuedMessage {
            content: text.to_string(),
            model_transition: staged_model.map(|c| ModelDirective::SwitchTo(Box::new(c))),
            mode_transition: staged_mode,
        };
        if self.agent.busy || self.queue.abort_pending {
            self.queue.messages.push_back(qm);
            self.queue.selected = Some(self.queue.messages.len() - 1);
        } else {
            self.sync_nvim_buffer_to_segments().await;
            let history = messages_for_resubmit(&self.chat.segments);
            self.chat
                .segments
                .push(ChatSegment::Message(Message::user(text)));
            self.save_history_async();
            self.rerender_chat().await;
            self.scroll_to_bottom();
            self.send_resubmit_to_agent(history, qm).await;
        }
        false
    }

    /// Handle a slash command from the Neovim buffer (apply immediately, no staging).
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
            if !self.is_node_proxy {
                if let Some(model_str) = result.model_override {
                    let resolved = sven_model::resolve_model_from_config(&self.config, &model_str);
                    self.session.apply_model(resolved);
                }
                if let Some(mode) = result.mode_override {
                    self.session.apply_mode(mode);
                }
            }
        }
        false
    }

    pub(crate) async fn send_to_agent(&mut self, qm: QueuedMessage) {
        if let Some(tx) = &self.agent.tx {
            // In node-proxy mode the node owns model/mode; never forward overrides.
            let (model_override, mode_override) = if self.is_node_proxy {
                (None, None)
            } else {
                (
                    qm.model_transition.map(ModelDirective::into_model_config),
                    qm.mode_transition,
                )
            };
            let _ = tx
                .send(AgentRequest::Submit {
                    content: qm.content.clone(),
                    model_override,
                    mode_override,
                })
                .await;
            self.agent.busy = true;
            // First message in chat: request LLM-generated title (local agent only).
            if self.chat.segments.len() == 1
                && (self.chat_title == "New chat" || self.chat_title.is_empty())
                && !self.is_node_proxy
            {
                if let Some(tx) = &self.agent.tx {
                    let _ = tx
                        .send(AgentRequest::GenerateTitle {
                            user_text: qm.content,
                        })
                        .await;
                }
            }
        }
    }

    pub(crate) async fn send_resubmit_to_agent(
        &mut self,
        messages: Vec<Message>,
        qm: QueuedMessage,
    ) {
        if let Some(tx) = &self.agent.tx {
            let (model_override, mode_override) = if self.is_node_proxy {
                (None, None)
            } else {
                (
                    qm.model_transition.map(ModelDirective::into_model_config),
                    qm.mode_transition,
                )
            };
            let is_first_message = messages.is_empty()
                && (self.chat_title == "New chat" || self.chat_title.is_empty())
                && !self.is_node_proxy;
            let _ = tx
                .send(AgentRequest::Resubmit {
                    messages,
                    new_user_content: qm.content.clone(),
                    model_override,
                    mode_override,
                })
                .await;
            self.agent.busy = true;
            if is_first_message {
                if let Some(tx) = &self.agent.tx {
                    let _ = tx
                        .send(AgentRequest::GenerateTitle {
                            user_text: qm.content,
                        })
                        .await;
                }
            }
        }
    }

    /// If the agent is currently idle and there are queued messages waiting,
    /// dequeue the first one and send it.
    pub(crate) async fn try_dequeue_next(&mut self) {
        if !self.agent.busy && self.edit.queue_index.is_none() && !self.queue.abort_pending {
            if let Some(next) = self.queue.messages.pop_front() {
                self.queue.selected = self
                    .queue
                    .selected
                    .map(|s| s.saturating_sub(1))
                    .filter(|_| !self.queue.messages.is_empty());
                if self.queue.messages.is_empty() && self.ui.focus == FocusPane::Queue {
                    self.ui.focus = FocusPane::Input;
                }
                self.chat
                    .segments
                    .push(ChatSegment::Message(Message::user(&next.content)));
                self.rerender_chat().await;
                self.chat.auto_scroll = true;
                self.scroll_to_bottom();
                self.send_to_agent(next).await;
            }
        }
    }

    /// Signal the currently running agent turn to abort.
    pub(crate) async fn send_abort_signal(&self) {
        let sender = self.agent.cancel.lock().await.take();
        drop(sender);
    }

    /// Force-submit the queue item at `idx`.
    pub(crate) async fn force_submit_queued_message(&mut self, idx: usize) {
        if idx >= self.queue.messages.len() {
            return;
        }
        let qm = match self.queue.messages.remove(idx) {
            Some(qm) => qm,
            None => return,
        };
        self.queue.selected = if self.queue.messages.is_empty() {
            None
        } else {
            Some(idx.min(self.queue.messages.len() - 1))
        };
        if self.queue.messages.is_empty() && self.ui.focus == FocusPane::Queue {
            self.ui.focus = FocusPane::Input;
        }

        if self.agent.busy {
            self.queue.messages.push_front(qm);
            self.queue.selected = Some(0);
            self.queue.abort_pending = false;
            self.send_abort_signal().await;
        } else {
            self.queue.abort_pending = false;
            let history = messages_for_resubmit(&self.chat.segments);
            self.chat
                .segments
                .push(ChatSegment::Message(Message::user(&qm.content)));
            self.save_history_async();
            self.rerender_chat().await;
            self.chat.auto_scroll = true;
            self.scroll_to_bottom();
            self.send_resubmit_to_agent(history, qm).await;
        }
    }
}

// ── Integration tests ─────────────────────────────────────────────────────────
//
// These tests exercise the full "user input → AgentRequest channel" path through
// the real App + SessionState machinery with a mock agent receiver.

#[cfg(test)]
mod submit_integration_tests {
    use sven_config::AgentMode;

    use crate::agent::AgentRequest;
    use crate::app::App;
    use crate::keys::Action;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn resubmit_content(req: &AgentRequest) -> &str {
        match req {
            AgentRequest::Resubmit {
                new_user_content, ..
            } => new_user_content,
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    fn resubmit_model(req: &AgentRequest) -> Option<String> {
        match req {
            AgentRequest::Resubmit { model_override, .. } => model_override
                .as_ref()
                .map(|c| format!("{}/{}", c.provider, c.name)),
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    fn resubmit_mode(req: &AgentRequest) -> Option<AgentMode> {
        match req {
            AgentRequest::Resubmit { mode_override, .. } => *mode_override,
            other => panic!("expected Resubmit, got {:?}", other),
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

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

    #[tokio::test]
    async fn model_override_consumed_after_first_message() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("/model openai/gpt-4o");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;

        let first = rx.try_recv().expect("first request");
        assert_eq!(resubmit_model(&first).as_deref(), Some("openai/gpt-4o"));

        app.simulate_turn_complete();

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        let second = rx.try_recv().expect("second request");
        assert!(
            resubmit_model(&second).is_none(),
            "model override must not persist to second message"
        );
    }

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

    #[tokio::test]
    async fn quit_command_returns_true() {
        let (mut app, _rx) = App::for_testing();
        app.inject_input("/quit");
        let quit = app.dispatch_action(Action::Submit).await;
        assert!(quit, "/quit must return true to terminate the event loop");
    }

    #[tokio::test]
    async fn unknown_command_sends_nothing() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("/doesnotexist foo");
        let quit = app.dispatch_action(Action::Submit).await;
        assert!(!quit);
        assert!(
            rx.try_recv().is_err(),
            "unknown command must not send to agent"
        );
    }

    #[tokio::test]
    async fn busy_agent_queues_messages() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        assert!(app.is_agent_busy(), "agent should be busy after first send");

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        assert_eq!(
            app.queued_len(),
            1,
            "second message should be queued while agent busy"
        );
        assert!(
            rx.try_recv().is_err(),
            "no second request should reach agent yet"
        );
    }

    #[tokio::test]
    async fn queued_message_retains_model_override() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message");

        app.inject_input("/model anthropic/claude-opus-4-6");
        app.dispatch_action(Action::Submit).await;

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;

        assert_eq!(app.queued_len(), 1);
        assert_eq!(
            app.model_display(),
            "anthropic/claude-opus-4-6",
            "model_display should be promoted when override is consumed into queue"
        );
    }

    #[tokio::test]
    async fn edit_resubmit_applies_staged_model_override() {
        let (mut app, mut rx) = App::for_testing();

        let seg_idx = app.inject_chat_user_message("original message");

        app.inject_input("/model anthropic/claude-opus-4-6");
        app.dispatch_action(Action::Submit).await;
        assert!(
            rx.try_recv().is_err(),
            "/model alone must not send a request"
        );

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

    #[tokio::test]
    async fn edit_resubmit_applies_staged_mode_override() {
        let (mut app, mut rx) = App::for_testing();

        let seg_idx = app.inject_chat_user_message("do some research");

        app.inject_input("/mode research");
        app.dispatch_action(Action::Submit).await;
        assert!(
            rx.try_recv().is_err(),
            "/mode alone must not send a request"
        );

        app.start_editing_segment(seg_idx, "do some research — edited");
        app.dispatch_action(Action::EditMessageConfirm).await;

        let req = rx.try_recv().expect("edit-resubmit must send a request");
        assert_eq!(
            resubmit_mode(&req),
            Some(AgentMode::Research),
            "staged mode must be forwarded to the agent on edit-resubmit"
        );
    }

    #[tokio::test]
    async fn empty_input_sends_nothing() {
        let (mut app, mut rx) = App::for_testing();
        app.inject_input("   ");
        app.dispatch_action(Action::Submit).await;
        assert!(
            rx.try_recv().is_err(),
            "empty/whitespace input must not send to agent"
        );
    }

    #[tokio::test]
    async fn abort_command_sets_abort_pending_when_busy() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");
        assert!(app.is_agent_busy());

        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        assert!(
            app.is_abort_pending(),
            "abort_pending must be set after /abort"
        );
    }

    #[tokio::test]
    async fn abort_pending_queues_new_messages_when_idle() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        app.simulate_aborted("partial response").await;
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        assert!(app.is_abort_pending());
        assert!(!app.is_agent_busy());

        app.inject_input("new message after abort");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(
            app.queued_len(),
            1,
            "message should be queued when abort_pending"
        );
        assert!(
            rx.try_recv().is_err(),
            "message must not go to agent directly"
        );
    }

    #[tokio::test]
    async fn abort_pending_suppresses_auto_dequeue() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first message sent");

        app.inject_input("second");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(app.queued_len(), 1, "second should be queued");

        app.simulate_aborted("").await;
        let _ = app.dispatch_action(Action::InterruptAgent).await;
        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        assert!(app.is_abort_pending());

        assert_eq!(
            app.queued_len(),
            1,
            "queue should not have been drained while abort_pending"
        );
        assert!(
            rx.try_recv().is_err(),
            "no message should have been sent automatically"
        );
    }

    #[tokio::test]
    async fn force_submit_while_idle_sends_immediately() {
        let (mut app, mut rx) = App::for_testing();

        app.inject_input("first");
        app.dispatch_action(Action::Submit).await;
        let _first = rx.try_recv().expect("first sent");

        app.inject_input("queued message");
        app.dispatch_action(Action::Submit).await;
        assert_eq!(app.queued_len(), 1);

        app.inject_input("/abort");
        app.dispatch_action(Action::Submit).await;
        app.simulate_aborted("").await;

        assert!(app.is_abort_pending());
        assert_eq!(app.queued_len(), 1);

        app.dispatch_action(Action::FocusQueue).await;
        app.dispatch_action(Action::ForceSubmitQueuedMessage).await;

        let req = rx.try_recv().expect("force-submit should send a request");
        assert_eq!(resubmit_content(&req), "queued message");
        assert_eq!(
            app.queued_len(),
            0,
            "queue should be empty after force-submit"
        );
    }
}
