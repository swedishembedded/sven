// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Test-only `App` constructors and state-injection helpers, shared by the
//! `#[cfg(test)]` suites in `submit.rs` and elsewhere in the crate.

use std::sync::Arc;

use crate::{
    agent::AgentRequest,
    app::{ui_state::FocusPane, App, AppOptions},
    keys::Action,
};

#[cfg(test)]
impl App {
    pub fn for_testing() -> (Self, tokio::sync::mpsc::Receiver<AgentRequest>) {
        let config = Arc::new(sven_config::Config::default());
        let opts = AppOptions {
            mode: sven_config::AgentMode::Agent,
            initial_prompt: None,
            initial_history: None,
            no_nvim: true,
            model_override: None,
            trace_path: None,
            load_trace_path: None,
            initial_queue: Vec::new(),
            node_backend: None,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut app = Self::new(config, opts);
        app.agent.tx = Some(tx);
        (app, rx)
    }

    pub fn inject_input(&mut self, text: &str) {
        self.input.buffer = text.to_string();
        self.input.cursor = text.len();
    }

    pub fn input_buffer_for_test(&self) -> &str {
        &self.input.buffer
    }

    pub async fn dispatch_action(&mut self, action: Action) -> bool {
        self.dispatch(action).await
    }

    pub fn is_agent_busy(&self) -> bool {
        self.agent.busy
    }

    pub fn queued_len(&self) -> usize {
        self.queue.messages.len()
    }

    pub fn model_display(&self) -> &str {
        &self.session.model_display
    }

    pub fn simulate_turn_complete(&mut self) {
        self.agent.busy = false;
    }

    pub fn inject_chat_user_message(&mut self, text: &str) -> usize {
        let idx = self.chat.segments.len();
        self.chat
            .segments
            .push(crate::chat::segment::ChatSegment::Message(
                sven_model::Message::user(text),
            ));
        idx
    }

    pub fn start_editing_segment(&mut self, seg_idx: usize, new_text: &str) {
        self.edit.message_index = Some(seg_idx);
        self.edit.buffer = new_text.to_string();
        self.edit.cursor = new_text.len();
        self.edit.original_text = Some(new_text.to_string());
        self.ui.focus = FocusPane::Input;
    }

    pub fn is_abort_pending(&self) -> bool {
        self.queue.abort_pending
    }

    pub async fn simulate_aborted(&mut self, partial_text: &str) {
        use crate::chat::segment::ChatSegment;
        use sven_model::Message;

        self.chat.streaming_buffer.clear();
        self.chat.streaming_is_thinking = false;
        if !partial_text.is_empty() {
            self.chat
                .segments
                .push(ChatSegment::Message(Message::assistant(partial_text)));
        }
        self.agent.busy = false;
        self.agent.current_tool = None;
    }
}
