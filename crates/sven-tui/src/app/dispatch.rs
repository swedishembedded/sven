// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Action dispatcher: maps every `Action` variant to `App` state mutations.

use sven_model::{Message, MessageContent, Role};

use crate::{
    app::{App, FocusPane, QueuedMessage},
    chat::{
        markdown::parse_markdown_to_messages,
        segment::{
            messages_for_resubmit, segment_at_line, segment_editable_text, segment_tool_call_id,
            ChatSegment,
        },
    },
    commands::{completion::CompletionItem, parse, CommandContext, ParsedCommand},
    keys::Action,
    overlay::completion::CompletionOverlay,
    overlay::confirm::{ConfirmModal, ConfirmedAction},
    pager::PagerOverlay,
};

/// Plain-text help for the chat pane shortcuts modal (Enter in chat).
const CHAT_HELP_MESSAGE: &str = "\
Navigation
  j / k       Move highlight down / up

Actions (apply to the highlighted message)
  e / Enter   Edit message
  y           Copy segment to clipboard
  Y           Copy all to clipboard
  x           Remove segment
  d           Truncate chat from here
  r           Rerun from this segment

Scrolling
  ^u / ^d     Page up / down
  g / G       Top / bottom

Other
  /           Search
  q           Focus queue panel
  Space       Toggle delegate summary
  ?           Show this help";

impl App {
    // ── Action dispatcher ─────────────────────────────────────────────────────

    pub(crate) async fn dispatch(&mut self, action: Action) -> bool {
        // Route input-manipulation actions to the edit buffer whenever we are in
        // any edit mode — both chat-segment edits and queue-item edits.
        if self.edit.active() {
            if let Some((buf, cur)) = self.apply_input_to_edit(&action) {
                self.edit.buffer = buf;
                self.edit.cursor = cur;
                // Live-preview only makes sense for chat segments (not queue items).
                if self.edit.message_index.is_some() {
                    self.update_editing_segment_live();
                    self.rerender_chat().await;
                }
                return false;
            }
        }

        match action {
            Action::FocusInput => {
                self.ui.focus = FocusPane::Input;
            }
            Action::NavUp => match self.ui.focus {
                FocusPane::Input => {
                    if !self.queue.messages.is_empty() {
                        if self.queue.selected.is_none() {
                            self.queue.selected = Some(0);
                        }
                        self.ui.focus = FocusPane::Queue;
                    } else {
                        self.ui.focus = FocusPane::Chat;
                        self.recompute_focused_segment();
                    }
                }
                FocusPane::Queue => {
                    self.ui.focus = FocusPane::Chat;
                    self.recompute_focused_segment();
                }
                FocusPane::Chat | FocusPane::ChatList | FocusPane::Peers => {}
            },
            Action::NavDown => match self.ui.focus {
                FocusPane::Chat | FocusPane::ChatList => {
                    if !self.queue.messages.is_empty() {
                        if self.queue.selected.is_none() {
                            self.queue.selected = Some(0);
                        }
                        self.ui.focus = FocusPane::Queue;
                    } else {
                        self.ui.focus = FocusPane::Input;
                    }
                }
                FocusPane::Queue => {
                    self.ui.focus = FocusPane::Input;
                }
                FocusPane::Input | FocusPane::Peers => {}
            },
            Action::NavLeft => {
                if self.ui.focus == FocusPane::ChatList {
                    self.ui.focus = FocusPane::Chat;
                    self.recompute_focused_segment();
                }
            }
            Action::NavRight => {
                if self.ui.focus != FocusPane::ChatList {
                    if !self.prefs.chat_list_visible {
                        self.prefs.chat_list_visible = true;
                    }
                    self.ui.focus = FocusPane::ChatList;
                    self.sessions.sync_list_selection_to_active();
                }
            }
            Action::FocusQueue => {
                if !self.queue.messages.is_empty() {
                    if self.queue.selected.is_none() {
                        self.queue.selected = Some(0);
                    }
                    self.ui.focus = FocusPane::Queue;
                }
            }
            Action::QueueNavUp => {
                if let Some(sel) = self.queue.selected {
                    self.queue.selected = Some(sel.saturating_sub(1));
                } else if !self.queue.messages.is_empty() {
                    self.queue.selected = Some(0);
                }
            }
            Action::QueueNavDown => {
                let len = self.queue.messages.len();
                if len > 0 {
                    let sel = self.queue.selected.unwrap_or(0);
                    self.queue.selected = Some((sel + 1).min(len - 1));
                }
            }
            Action::QueueEditSelected => {
                if let Some(idx) = self.queue.selected {
                    if let Some(qm) = self.queue.messages.get(idx) {
                        let text = qm.content.clone();
                        self.edit.queue_index = Some(idx);
                        self.edit.cursor = text.len();
                        self.edit.original_text = Some(text.clone());
                        self.edit.buffer = text;
                        self.ui.focus = FocusPane::Input;
                    }
                }
            }

            Action::EditMessageAtCursor => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    if let Some(text) = segment_editable_text(&self.chat.segments, seg_idx) {
                        self.edit.message_index = Some(seg_idx);
                        self.edit.cursor = text.len();
                        self.edit.original_text = Some(text.clone());
                        self.edit.buffer = text;
                        self.ui.focus = FocusPane::Input;
                        self.update_editing_segment_live();
                        self.rerender_chat().await;
                    }
                }
            }

            Action::DeleteChatSegment => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    if self
                        .edit
                        .message_index
                        .map(|i| i >= seg_idx)
                        .unwrap_or(false)
                    {
                        self.edit.clear();
                    }
                    self.chat.segments.truncate(seg_idx);
                    self.chat.expand_level.retain(|&i, _| i < seg_idx);
                    self.chat.focused_segment = None;
                    self.rerender_chat().await;
                    self.save_history_async();
                }
            }

            Action::RemoveChatSegment => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    let paired_id: Option<String> = self
                        .chat
                        .segments
                        .get(seg_idx)
                        .and_then(segment_tool_call_id)
                        .map(String::from);

                    let mut to_remove: Vec<usize> = vec![seg_idx];
                    if let Some(ref call_id) = paired_id {
                        for (i, seg) in self.chat.segments.iter().enumerate() {
                            if i != seg_idx && segment_tool_call_id(seg) == Some(call_id.as_str()) {
                                to_remove.push(i);
                            }
                        }
                    }
                    to_remove.sort_unstable_by(|a, b| b.cmp(a));

                    if self
                        .edit
                        .message_index
                        .map(|i| to_remove.contains(&i))
                        .unwrap_or(false)
                    {
                        self.edit.clear();
                    }

                    for idx in &to_remove {
                        if *idx < self.chat.segments.len() {
                            self.chat.segments.remove(*idx);
                        }
                    }

                    let min_removed = *to_remove.last().unwrap_or(&seg_idx);
                    let removed_count = to_remove.len();
                    self.chat.expand_level = self
                        .chat
                        .expand_level
                        .iter()
                        .filter_map(|(&i, &level)| {
                            if to_remove.contains(&i) {
                                None
                            } else if i > min_removed {
                                Some((i - removed_count, level))
                            } else {
                                Some((i, level))
                            }
                        })
                        .collect();

                    self.chat.focused_segment = None;
                    self.rerender_chat().await;
                    self.save_history_async();
                }
            }

            Action::CopySegment => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    if self.copy_segment_to_clipboard(seg_idx) {
                        self.ui
                            .push_toast(crate::app::ui_state::Toast::info("Copied to clipboard"));
                    }
                }
            }

            Action::CopyAll => {
                if self.copy_all_to_clipboard() {
                    self.ui
                        .push_toast(crate::app::ui_state::Toast::info("Copied all to clipboard"));
                }
            }

            Action::RerunFromSegment => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    let last_user =
                        (0..seg_idx)
                            .rev()
                            .find_map(|i| match self.chat.segments.get(i) {
                                Some(ChatSegment::Message(m)) => {
                                    if matches!(
                                        (&m.role, &m.content),
                                        (
                                            sven_model::Role::User,
                                            sven_model::MessageContent::Text(_)
                                        )
                                    ) {
                                        match &m.content {
                                            sven_model::MessageContent::Text(t) => {
                                                Some((i, t.clone()))
                                            }
                                            _ => None,
                                        }
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            });

                    if let Some((user_idx, user_text)) = last_user {
                        self.edit.clear();
                        self.chat.segments.truncate(user_idx);
                        self.chat.expand_level.retain(|&i, _| i < user_idx);
                        let messages = messages_for_resubmit(&self.chat.segments);
                        self.chat
                            .segments
                            .push(ChatSegment::Message(sven_model::Message::user(&user_text)));
                        self.chat.focused_segment = None;
                        let qm = QueuedMessage {
                            content: user_text,
                            model_transition: None,
                            mode_transition: None,
                        };
                        self.rerender_chat().await;
                        self.chat.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_resubmit_to_agent(messages, qm).await;
                    }
                }
            }

            Action::DeleteQueuedMessage => {
                if let Some(idx) = self.queue.selected {
                    if idx < self.queue.messages.len() {
                        if self.edit.queue_index == Some(idx) {
                            self.edit.clear();
                        }
                        self.queue.messages.remove(idx);
                        if self.queue.messages.is_empty() {
                            self.queue.selected = None;
                            if self.ui.focus == FocusPane::Queue {
                                self.ui.focus = FocusPane::Input;
                            }
                        } else {
                            self.queue.selected = Some(idx.min(self.queue.messages.len() - 1));
                        }
                    }
                }
            }

            Action::EditMessageConfirm => {
                // Queue-item edit confirm.
                if let Some(q_idx) = self.edit.queue_index {
                    let new_content = self.edit.buffer.trim().to_string();
                    self.edit.clear();
                    if !new_content.is_empty() {
                        if let Some(entry) = self.queue.messages.get_mut(q_idx) {
                            entry.content = new_content;
                        }
                    }
                    self.ui.focus = if self.queue.messages.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    self.try_dequeue_next().await;
                    return false;
                }
                // Chat-segment edit confirm.
                if let Some(i) = self.edit.message_index {
                    let new_content = self.edit.buffer.trim().to_string();
                    self.edit.clear();
                    if new_content.is_empty() {
                        return false;
                    }
                    let seg = match self.chat.segments.get(i) {
                        Some(ChatSegment::Message(m)) => m.clone(),
                        _ => return false,
                    };
                    match (&seg.role, &seg.content) {
                        (Role::User, MessageContent::Text(_)) => {
                            let (staged_model, staged_mode) = self.session.consume_staged();
                            let qm = crate::app::QueuedMessage {
                                content: new_content.clone(),
                                model_transition: staged_model
                                    .map(|c| crate::app::ModelDirective::SwitchTo(Box::new(c))),
                                mode_transition: staged_mode,
                            };
                            self.chat.segments.truncate(i + 1);
                            self.chat.segments.pop();
                            self.chat
                                .segments
                                .push(ChatSegment::Message(Message::user(&new_content)));
                            let messages = messages_for_resubmit(&self.chat.segments);
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, qm).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            if let Some(ChatSegment::Message(m)) = self.chat.segments.get_mut(i) {
                                m.content = MessageContent::Text(new_content);
                            }
                            self.build_display_from_segments();
                            self.ui.search.update_matches(&self.chat.lines);
                            self.rerender_chat().await;
                            self.save_history_async();
                        }
                        _ => {}
                    }
                }
            }

            // ESC in the normal input pane (not triggered from completion overlay
            // which is handled earlier in term_events.rs).
            //
            // Priority:
            //   1. An inline edit is in progress → cancel it (restore original).
            //   2. Input box has content / attachments → clear it.
            //   3. Already empty → do nothing.
            Action::InputEscape => {
                if self.edit.active() {
                    // Cancel an in-progress inline edit (same logic as EditMessageCancel).
                    if self.edit.queue_index.is_some() {
                        if let (Some(q_idx), Some(original)) =
                            (self.edit.queue_index, self.edit.original_text.clone())
                        {
                            if let Some(entry) = self.queue.messages.get_mut(q_idx) {
                                entry.content = original;
                            }
                        }
                        self.edit.clear();
                        self.ui.focus = if self.queue.messages.is_empty() {
                            FocusPane::Input
                        } else {
                            FocusPane::Queue
                        };
                        self.try_dequeue_next().await;
                        return false;
                    }
                    if let Some(idx) = self.edit.message_index {
                        if let Some(original) = self.edit.original_text.clone() {
                            if let Some(ChatSegment::Message(m)) = self.chat.segments.get_mut(idx) {
                                match (&m.role, &mut m.content) {
                                    (Role::User, MessageContent::Text(t)) => *t = original,
                                    (Role::Assistant, MessageContent::Text(t)) => *t = original,
                                    _ => {}
                                }
                            }
                            self.build_display_from_segments();
                            self.ui.search.update_matches(&self.chat.lines);
                        }
                    }
                    self.edit.clear();
                    return false;
                }
                // No active edit: clear the input box completely.
                self.input.buffer.clear();
                self.input.cursor = 0;
                self.input.scroll_offset = 0;
                self.input.attachments.clear();
                self.input.history_idx = None;
                self.input.history_draft = None;
                self.ui.completion = None;
            }

            Action::EditMessageCancel => {
                // Cancel queue-item edit — restore original text if available.
                if self.edit.queue_index.is_some() {
                    if let (Some(q_idx), Some(original)) =
                        (self.edit.queue_index, self.edit.original_text.clone())
                    {
                        if let Some(entry) = self.queue.messages.get_mut(q_idx) {
                            entry.content = original;
                        }
                    }
                    self.edit.clear();
                    self.ui.focus = if self.queue.messages.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    self.try_dequeue_next().await;
                    return false;
                }
                // Cancel chat-segment edit.
                if let Some(idx) = self.edit.message_index {
                    if let Some(original) = self.edit.original_text.clone() {
                        if let Some(ChatSegment::Message(m)) = self.chat.segments.get_mut(idx) {
                            match (&m.role, &mut m.content) {
                                (Role::User, MessageContent::Text(t)) => *t = original,
                                (Role::Assistant, MessageContent::Text(t)) => *t = original,
                                _ => {}
                            }
                        }
                        self.build_display_from_segments();
                        self.ui.search.update_matches(&self.chat.lines);
                    }
                }
                self.edit.clear();
            }

            Action::SubmitBufferToAgent => {
                // Ignore submit signals that arrive while the agent is already
                // running (e.g. a stray :w during streaming).
                if self.agent.busy {
                    return false;
                }
                if let Some(nvim_bridge) = &self.nvim.bridge {
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
                            let trimmed = new_user_content.trim();
                            if trimmed.starts_with('/') {
                                return self.submit_nvim_command(trimmed).await;
                            }
                            self.chat.segments = messages
                                .iter()
                                .map(|m| ChatSegment::Message(m.clone()))
                                .collect();
                            self.chat.tool_args.clear();
                            for msg in &messages {
                                if let MessageContent::ToolCall {
                                    tool_call_id,
                                    function,
                                } = &msg.content
                                {
                                    self.chat
                                        .tool_args
                                        .insert(tool_call_id.clone(), function.name.clone());
                                }
                            }
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            // Strip the trailing user message from the history
                            // because `replace_history_and_submit` will re-append
                            // it via `new_user_content`.  Without this the user
                            // message appears twice ([..., User:"Hi", User:"Hi"]),
                            // which causes the model to treat it as a new prompt
                            // after the tool-use round and generate an extra reply.
                            let last_user_pos = messages.iter().rposition(|m| m.role == Role::User);
                            let history = if let Some(pos) = last_user_pos {
                                messages[..pos].to_vec()
                            } else {
                                messages
                            };
                            self.send_resubmit_to_agent(
                                history,
                                QueuedMessage::plain(new_user_content),
                            )
                            .await;
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

            Action::ChatHighlightDown => {
                if self.nvim.bridge.is_some() {
                    return false;
                }
                let n = self.chat.segments.len();
                if n == 0 {
                    return false;
                }
                let next = match self.chat.focused_segment {
                    Some(i) => (i + 1).min(n - 1),
                    None => 0,
                };
                self.chat.focused_segment = Some(next);
                self.scroll_chat_to_show_segment(next);
            }
            Action::ChatHighlightUp => {
                if self.nvim.bridge.is_some() {
                    return false;
                }
                let n = self.chat.segments.len();
                if n == 0 {
                    return false;
                }
                let prev = match self.chat.focused_segment {
                    Some(i) => i.saturating_sub(1),
                    None => n - 1,
                };
                self.chat.focused_segment = Some(prev);
                self.scroll_chat_to_show_segment(prev);
            }
            Action::ShowChatHelp => {
                self.ui.confirm_modal = Some(ConfirmModal::info_with_border(
                    "Chat shortcuts",
                    CHAT_HELP_MESSAGE,
                    ratatui::style::Color::Green,
                ));
            }
            Action::ScrollUp => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-y>").await;
                } else {
                    self.scroll_up(1);
                }
            }
            Action::ScrollDown => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-e>").await;
                } else {
                    self.scroll_down(1);
                }
            }
            Action::ScrollPageUp => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-u>").await;
                } else {
                    self.scroll_up(self.layout.chat_height / 2);
                }
            }
            Action::ScrollPageDown => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-d>").await;
                } else {
                    self.scroll_down(self.layout.chat_height / 2);
                }
            }
            Action::ScrollTop => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("gg").await;
                } else {
                    self.chat.scroll_offset = 0;
                    self.chat.auto_scroll = false;
                }
            }
            Action::ScrollBottom => {
                self.chat.auto_scroll = true;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
            }

            Action::SearchOpen => {
                self.ui.search.query.clear();
                self.ui.search.current = 0;
                self.ui.search.update_matches(&self.chat.lines);
                self.ui.search.active = true;
                self.ui.focus = FocusPane::Chat;
                self.recompute_focused_segment();
            }
            Action::SearchClose => {
                self.ui.search.active = false;
                if let Some(line) = self.ui.search.current_line() {
                    if self.ui.inspector.is_some() {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_to_line(line);
                        }
                    } else if let Some(pager) = &mut self.ui.pager {
                        pager.scroll_to_line(line);
                    } else {
                        self.chat.scroll_offset = line as u16;
                    }
                }
            }
            Action::SearchInput(c) => {
                self.ui.search.query.push(c);
                if self.ui.inspector.is_some() {
                    // Search scoped to the inspector's own content.
                    let lines = self
                        .ui
                        .inspector
                        .as_ref()
                        .map(|i| i.pager.cloned_lines())
                        .unwrap_or_default();
                    self.ui.search.update_matches(&lines);
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_to_line(line);
                        }
                    }
                } else {
                    self.ui.search.update_matches(&self.chat.lines);
                    if let Some(line) = self.ui.search.current_line() {
                        self.chat.scroll_offset = line as u16;
                        if let Some(pager) = &mut self.ui.pager {
                            pager.scroll_to_line(line);
                        }
                    }
                }
            }
            Action::SearchBackspace => {
                self.ui.search.query.pop();
                if self.ui.inspector.is_some() {
                    let lines = self
                        .ui
                        .inspector
                        .as_ref()
                        .map(|i| i.pager.cloned_lines())
                        .unwrap_or_default();
                    self.ui.search.update_matches(&lines);
                    if let Some(line) = self.ui.search.current_line() {
                        if let Some(insp) = &mut self.ui.inspector {
                            insp.pager.scroll_to_line(line);
                        }
                    }
                } else {
                    self.ui.search.update_matches(&self.chat.lines);
                }
            }
            Action::SearchNextMatch => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current =
                        (self.ui.search.current + 1) % self.ui.search.matches.len();
                    if let Some(line) = self.ui.search.current_line() {
                        if self.ui.inspector.is_some() {
                            if let Some(insp) = &mut self.ui.inspector {
                                insp.pager.scroll_to_line(line);
                            }
                        } else {
                            self.chat.scroll_offset = line as u16;
                            if let Some(pager) = &mut self.ui.pager {
                                pager.scroll_to_line(line);
                            }
                        }
                    }
                }
            }
            Action::SearchPrevMatch => {
                if !self.ui.search.matches.is_empty() {
                    self.ui.search.current = self
                        .ui
                        .search
                        .current
                        .checked_sub(1)
                        .unwrap_or(self.ui.search.matches.len() - 1);
                    if let Some(line) = self.ui.search.current_line() {
                        if self.ui.inspector.is_some() {
                            if let Some(insp) = &mut self.ui.inspector {
                                insp.pager.scroll_to_line(line);
                            }
                        } else {
                            self.chat.scroll_offset = line as u16;
                            if let Some(pager) = &mut self.ui.pager {
                                pager.scroll_to_line(line);
                            }
                        }
                    }
                }
            }

            Action::InputChar(c) => {
                self.input.buffer.insert(self.input.cursor, c);
                self.input.cursor += c.len_utf8();
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.ui.completion = None;
                }
            }
            Action::InputNewline => {
                self.input.buffer.insert(self.input.cursor, '\n');
                self.input.cursor += 1;
                self.ui.completion = None;
            }
            Action::InputBackspace => {
                if self.input.cursor > 0 {
                    let prev = prev_char_boundary(&self.input.buffer, self.input.cursor);
                    self.input.buffer.remove(prev);
                    self.input.cursor = prev;
                }
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.ui.completion = None;
                }
            }
            Action::InputDelete => {
                if self.input.cursor < self.input.buffer.len() {
                    self.input.buffer.remove(self.input.cursor);
                }
            }
            Action::InputMoveCursorLeft => {
                self.input.cursor = prev_char_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveCursorRight => {
                if self.input.cursor < self.input.buffer.len() {
                    let ch = self.input.buffer[self.input.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                    self.input.cursor += ch;
                }
            }
            Action::InputMoveWordLeft => {
                self.input.cursor = prev_word_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveWordRight => {
                self.input.cursor = next_word_boundary(&self.input.buffer, self.input.cursor);
            }
            Action::InputMoveLineStart => self.input.cursor = 0,
            Action::InputMoveLineEnd => self.input.cursor = self.input.buffer.len(),
            Action::InputMoveLineUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
                    if ws.cursor_row > 0 {
                        // Move cursor up within the multi-line text.
                        self.input.cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input.buffer,
                            w,
                            ws.cursor_row - 1,
                            ws.cursor_col,
                        );
                    } else {
                        // Already on the first visual row — cycle to the older history entry.
                        if let Some(entry) = self.input.history_up() {
                            let text = entry.to_string();
                            self.input.cursor = text.len();
                            self.input.buffer = text;
                            self.input.scroll_offset = 0;
                        }
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        // Move cursor down within the multi-line text.
                        self.input.cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input.buffer,
                            w,
                            ws.cursor_row + 1,
                            ws.cursor_col,
                        );
                    } else {
                        // Already on the last visual row — cycle to the newer history entry.
                        if let Some(entry) = self.input.history_down() {
                            let text = entry.to_string();
                            self.input.cursor = text.len();
                            self.input.buffer = text;
                            self.input.scroll_offset = 0;
                        }
                    }
                }
            }
            Action::InputPageUp => {
                let h = self.layout.input_inner_height as usize;
                if self.edit.active() {
                    self.edit.scroll_offset = self.edit.scroll_offset.saturating_sub(h);
                } else {
                    self.input.scroll_offset = self.input.scroll_offset.saturating_sub(h);
                }
            }
            Action::InputPageDown => {
                let w = self.layout.input_inner_width as usize;
                let h = self.layout.input_inner_height as usize;
                if w > 0 && h > 0 {
                    let in_edit = self.edit.active();
                    let content = if in_edit {
                        &self.edit.buffer
                    } else {
                        &self.input.buffer
                    };
                    let ws = crate::input_wrap::wrap_content(content, w, 0);
                    let max = ws.lines.len().saturating_sub(h);
                    if in_edit {
                        self.edit.scroll_offset = (self.edit.scroll_offset + h).min(max);
                    } else {
                        self.input.scroll_offset = (self.input.scroll_offset + h).min(max);
                    }
                }
            }
            Action::InputDeleteToEnd => self.input.buffer.truncate(self.input.cursor),
            Action::InputDeleteToStart => {
                self.input.buffer = self.input.buffer[self.input.cursor..].to_string();
                self.input.cursor = 0;
            }

            Action::InputHistoryUp => {
                if let Some(entry) = self.input.history_up() {
                    let text = entry.to_string();
                    self.input.cursor = text.len();
                    self.input.buffer = text;
                    self.input.scroll_offset = 0;
                }
                return false;
            }

            Action::InputHistoryDown => {
                if let Some(entry) = self.input.history_down() {
                    let text = entry.to_string();
                    self.input.cursor = text.len();
                    self.input.buffer = text;
                    self.input.scroll_offset = 0;
                }
                return false;
            }

            Action::ResizeInputGrow => {
                self.prefs.input_height = (self.prefs.input_height + 1).min(20);
            }

            Action::ResizeInputShrink => {
                self.prefs.input_height = (self.prefs.input_height - 1).max(3);
            }

            Action::Submit => {
                self.ui.completion = None;
                let text = std::mem::take(&mut self.input.buffer).trim().to_string();
                self.input.cursor = 0;
                self.input.scroll_offset = 0;
                if text.is_empty() && self.input.attachments.is_empty() {
                    return false;
                }
                // Prepend attachment paths to the submitted text.
                let full_text = if self.input.attachments.is_empty() {
                    text.clone()
                } else {
                    let att_text: String = self
                        .input
                        .attachments
                        .iter()
                        .map(|a| a.to_message_text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() {
                        att_text
                    } else {
                        format!("{att_text}\n{text}")
                    }
                };
                self.input.attachments.clear();
                // Save to history (only the user-typed text, not the attachment metadata).
                if !text.is_empty() {
                    self.input.push_history(&text);
                }
                if full_text.is_empty() {
                    return false;
                }
                return self.submit_user_input(&full_text).await;
            }

            Action::CompletionNext => {
                if let Some(overlay) = &mut self.ui.completion {
                    overlay.select_next();
                } else if self.should_show_completion() {
                    self.update_completion_overlay();
                }
            }
            Action::CompletionPrev => {
                if let Some(overlay) = &mut self.ui.completion {
                    overlay.select_prev();
                }
            }
            Action::CompletionSelect => {
                if let Some(overlay) = self.ui.completion.take() {
                    if let Some(item) = overlay.selected_item() {
                        let item = item.clone();
                        self.apply_completion(&item);
                        // If the applied completion produced a slash command, submit it
                        // immediately so the user only has to press Enter once.
                        if self.input.buffer.trim().starts_with('/') {
                            self.ui.completion = None;
                            let text = std::mem::take(&mut self.input.buffer).trim().to_string();
                            self.input.cursor = 0;
                            self.input.scroll_offset = 0;
                            if text.is_empty() && self.input.attachments.is_empty() {
                                return false;
                            }
                            let full_text = if self.input.attachments.is_empty() {
                                text.clone()
                            } else {
                                let att_text: String = self
                                    .input
                                    .attachments
                                    .iter()
                                    .map(|a| a.to_message_text())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if text.is_empty() {
                                    att_text
                                } else {
                                    format!("{att_text}\n{text}")
                                }
                            };
                            self.input.attachments.clear();
                            if !text.is_empty() {
                                self.input.push_history(&text);
                            }
                            if !full_text.is_empty() {
                                return self.submit_user_input(&full_text).await;
                            }
                        }
                    }
                }
            }
            Action::CompletionCancel => {
                self.ui.completion = None;
            }

            Action::InterruptAgent => {
                if self.agent.busy {
                    self.queue.abort_pending = true;
                    self.send_abort_signal().await;
                }
            }

            Action::ForceSubmitQueuedMessage => {
                if let Some(idx) = self.queue.selected {
                    self.force_submit_queued_message(idx).await;
                }
            }

            Action::QueueSubmitSelected => {
                if let Some(idx) = self.queue.selected {
                    if !self.agent.busy && idx < self.queue.messages.len() {
                        self.queue.abort_pending = false;
                        if let Some(qm) = self.queue.messages.remove(idx) {
                            self.queue.selected = if self.queue.messages.is_empty() {
                                None
                            } else {
                                Some(idx.min(self.queue.messages.len() - 1))
                            };
                            if self.queue.messages.is_empty() && self.ui.focus == FocusPane::Queue {
                                self.ui.focus = FocusPane::Input;
                            }
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
            }

            Action::CycleMode => {
                if !self.is_node_proxy {
                    self.session.cycle_mode();
                }
            }

            Action::Help => {
                self.ui.show_help = !self.ui.show_help;
            }

            Action::OpenPager => {
                let mut pager = PagerOverlay::new(self.chat.lines.clone());
                if let Some(line) = self.ui.search.current_line() {
                    pager.scroll_to_line(line);
                }
                self.ui.pager = Some(pager);
            }

            // ── Team / multi-agent actions ────────────────────────────────────
            Action::OpenTeamPicker => {
                // Close any other overlay first.
                self.ui.show_help = false;
                // When no P2P team has been formed yet, seed the picker with a
                // self-entry so the overlay is usable and AgentPickerStatus
                // variants are exercised for display from day one.
                if self.ui.team_picker_entries.is_empty() {
                    use crate::ui::team_picker::{AgentPickerStatus, TeamPickerEntry};
                    self.ui.team_picker_entries.push(TeamPickerEntry {
                        name: "local".to_string(),
                        role: format!("{:?}", self.session.mode).to_lowercase(),
                        peer_id: String::new(),
                        status: AgentPickerStatus::Active,
                        current_task: None,
                        is_local: true,
                    });
                }
                self.ui.toggle_team_picker();
            }

            Action::TeamPickerNext => {
                self.ui.team_picker_next();
            }
            Action::TeamPickerPrev => {
                self.ui.team_picker_prev();
            }

            Action::TeamPickerSelect => {
                if self.ui.show_team_picker {
                    let peer_id = self.ui.team_picker_selected_peer().map(|s| s.to_string());
                    self.ui.active_session_peer = peer_id;
                    self.ui.show_team_picker = false;
                }
            }

            Action::TeamPickerClose => {
                self.ui.show_team_picker = false;
            }

            Action::CycleTeammateForward => {
                self.ui.cycle_teammate_view_forward();
            }

            Action::CycleTeammateBackward => {
                self.ui.cycle_teammate_view_backward();
            }

            Action::ToggleTaskList => {
                // Reuse the pager overlay with the current task list text.
                // TODO: render actual task list from the TaskStore.
                let placeholder = "Task list is not available in this session.\n\
                                   Connect to a team-enabled sven node to see task details.";
                if self.ui.pager.is_none() {
                    use crate::markdown::StyledLines;
                    let lines = StyledLines::from(vec![ratatui::text::Line::from(placeholder)]);
                    self.ui.pager = Some(PagerOverlay::new(lines));
                } else {
                    self.ui.pager = None;
                }
            }

            Action::ToggleDelegateSummary => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    if let Some(ChatSegment::DelegateSummary { expanded, .. }) =
                        self.chat.segments.get_mut(seg_idx)
                    {
                        *expanded = !*expanded;
                        self.rerender_chat().await;
                    }
                }
            }

            // ── Chat list sidebar actions ─────────────────────────────────────
            Action::ToggleChatList => {
                self.prefs.chat_list_visible = !self.prefs.chat_list_visible;
                // When hiding, move focus away from the now-invisible pane.
                if !self.prefs.chat_list_visible && self.ui.focus == FocusPane::ChatList {
                    self.ui.focus = FocusPane::Input;
                }
            }

            Action::FocusChatList => {
                if !self.prefs.chat_list_visible {
                    // Show the pane first, then focus it.
                    self.prefs.chat_list_visible = true;
                }
                self.ui.focus = FocusPane::ChatList;
                self.sessions.sync_list_selection_to_active();
            }

            Action::ChatListSelectNext => {
                self.sessions.select_next();
            }

            Action::ChatListSelectPrev => {
                self.sessions.select_prev();
            }

            Action::ChatListActivate => {
                if let Some(id) = self
                    .sessions
                    .tree_rows()
                    .get(self.sessions.list_selected)
                    .map(|(id, _)| id.clone())
                {
                    if id != self.sessions.active_id {
                        self.switch_session(id).await;
                    }
                    self.ui.focus = FocusPane::Input;
                }
            }

            Action::NewChat => {
                self.new_session().await;
            }

            Action::DeleteChat => {
                if let Some(id) = self
                    .sessions
                    .tree_rows()
                    .get(self.sessions.list_selected)
                    .map(|(id, _)| id.clone())
                {
                    let title = self
                        .sessions
                        .get(&id)
                        .map(|e| e.title.as_str())
                        .unwrap_or("Untitled");
                    let is_active = id == self.sessions.active_id;
                    let message = if is_active {
                        format!(
                            "Delete \"{}\"? You will be switched to another chat.",
                            title
                        )
                    } else {
                        format!("Delete \"{}\"?", title)
                    };
                    self.ui.confirm_modal = Some(
                        ConfirmModal::new("Delete chat", message, ConfirmedAction::DeleteChat(id))
                            .labels(" Delete ", " Cancel "),
                    );
                }
            }

            Action::ArchiveChat => {
                if let Some(id) = self
                    .sessions
                    .tree_rows()
                    .get(self.sessions.list_selected)
                    .map(|(id, _)| id.clone())
                {
                    self.sessions.archive(&id);
                    // Save the updated status to disk.
                    if let Some(entry) = self.sessions.get(&id) {
                        if let Some(path) = entry.yaml_path.clone() {
                            if path.exists() {
                                if let Ok(content) = std::fs::read_to_string(&path) {
                                    if let Ok(mut doc) = sven_input::parse_chat_document(&content) {
                                        doc.status = sven_input::ChatStatus::Archived;
                                        let _ = sven_input::save_chat_to(&path, &mut doc);
                                    }
                                }
                            }
                        }
                    }
                    self.ui
                        .push_toast(crate::app::ui_state::Toast::info("Chat archived"));
                }
            }

            Action::ResizeChatListGrow => {
                self.prefs.chat_list_grow();
            }

            Action::ResizeChatListShrink => {
                self.prefs.chat_list_shrink();
            }

            // ── Mouse-originated actions ──────────────────────────────────────
            Action::ChatListClick { inner_row } => {
                // `inner_row` is the 0-based visual row; add the scroll offset
                // that was in effect at render time to get the item index.
                let scroll_offset = self.chat_list_scroll_offset();
                let rows = self.sessions.tree_rows();
                let max_idx = rows.len().saturating_sub(1);
                let actual_idx = (inner_row + scroll_offset).min(max_idx);
                self.sessions.list_selected = actual_idx;
                if let Some((id, _)) = rows.get(actual_idx) {
                    if *id != self.sessions.active_id {
                        self.switch_session(id.clone()).await;
                    }
                }
                self.ui.focus = FocusPane::Input;
            }

            Action::ChatScrollbarClick { rel_row } => {
                let chat_inner_h = self.layout.chat_pane.height.saturating_sub(2);
                let total_chat_lines = self.chat.lines.len() as u16;
                if chat_inner_h > 0 && total_chat_lines > chat_inner_h {
                    let new_offset = (rel_row as u32 * (total_chat_lines - chat_inner_h) as u32
                        / chat_inner_h.saturating_sub(1).max(1) as u32)
                        as u16;
                    self.chat.scroll_offset =
                        new_offset.min(total_chat_lines.saturating_sub(chat_inner_h));
                    self.chat.auto_scroll = false;
                }
                // Scrollbar click clears any in-progress selection.
                self.chat.selection_anchor = None;
                self.chat.selection_end = None;
                self.chat.is_selecting = false;
            }

            Action::QueueClick { index } => {
                // Clear selection anchor (click is outside chat content).
                self.chat.selection_anchor = None;
                self.chat.selection_end = None;
                self.chat.is_selecting = false;

                if index < self.queue.messages.len() {
                    self.queue.selected = Some(index);
                    self.ui.focus = FocusPane::Queue;
                    if let Some(qm) = self.queue.messages.get(index) {
                        let text = qm.content.clone();
                        self.edit.queue_index = Some(index);
                        self.edit.cursor = text.len();
                        self.edit.original_text = Some(text.clone());
                        self.edit.buffer = text;
                        self.ui.focus = FocusPane::Input;
                    }
                }
            }

            Action::ChatContentClick {
                abs_line,
                inner_col,
            } => {
                // Set selection anchor for a potential drag.  Any previous
                // completed selection is cleared so it doesn't stay highlighted
                // while the user starts a new one.
                self.chat.selection_anchor = Some((abs_line, inner_col));
                self.chat.selection_end = None;
                self.chat.is_selecting = false;

                // Clear any open confirm modal if the click is outside the
                // icon area (the icon detection already happens in hit_test).
                // Since this action only fires for non-icon clicks, always clear.
                self.ui.confirm_modal = None;

                // Expand/collapse if the click lands on a collapsible segment.
                if let Some(seg_idx) = segment_at_line(&self.chat.segment_line_ranges, abs_line) {
                    let is_collapsible = match self.chat.segments.get(seg_idx) {
                        Some(ChatSegment::Message(m)) => matches!(
                            (&m.role, &m.content),
                            (Role::User, MessageContent::Text(_))
                                | (Role::Assistant, MessageContent::Text(_))
                                | (Role::Assistant, MessageContent::ToolCall { .. })
                                | (Role::Tool, MessageContent::ToolResult { .. })
                        ),
                        Some(ChatSegment::Thinking { .. }) => true,
                        _ => false,
                    };
                    if is_collapsible {
                        if let Some(seg) = self.chat.segments.get(seg_idx) {
                            let cur = self.chat.effective_expand_level(seg_idx, seg);
                            let next = if cur == 0 { 2 } else { 0 };
                            self.chat.expand_level.insert(seg_idx, next);
                            // When expanding a tool call, also expand the paired result
                            // so it is visible without an extra click.
                            if next >= 2 {
                                if let Some(result_idx) = self.paired_result_for(seg_idx) {
                                    let result_seg = self.chat.segments[result_idx].clone();
                                    let cur_result =
                                        self.chat.effective_expand_level(result_idx, &result_seg);
                                    if cur_result == 0 {
                                        self.chat.expand_level.insert(result_idx, 2);
                                    }
                                }
                            }
                        }
                        self.build_display_from_segments();
                        self.ui.search.update_matches(&self.chat.lines);
                        let max_offset =
                            (self.chat.lines.len() as u16).saturating_sub(self.layout.chat_height);
                        self.chat.scroll_offset = self.chat.scroll_offset.min(max_offset);
                        if let Some(&(seg_start, _)) = self.chat.segment_line_ranges.get(seg_idx) {
                            if (seg_start as u16) < self.chat.scroll_offset {
                                self.chat.scroll_offset = seg_start as u16;
                            }
                        }
                        self.chat.focused_segment = Some(seg_idx);
                    }
                }
            }

            Action::SelectionExtend {
                abs_line,
                inner_col,
                mouse_row,
            } => {
                let capped = abs_line.min(self.chat.lines.len().saturating_sub(1));
                self.chat.selection_end = Some((capped, inner_col));
                self.chat.is_selecting = true;

                // Auto-scroll when the pointer is near the top / bottom edge.
                let cp = self.layout.chat_pane;
                let content_top = cp.y + 1;
                let content_bottom = content_top + cp.height.saturating_sub(2);
                const SCROLL_ZONE: u16 = 2;
                if mouse_row < content_top + SCROLL_ZONE {
                    self.scroll_up(1);
                } else if mouse_row >= content_bottom.saturating_sub(SCROLL_ZONE) {
                    self.scroll_down(1);
                }
            }

            Action::SelectionFinish => {
                self.copy_selection_to_clipboard();
                // Keep anchor + end so the selection stays highlighted until
                // the next mouse-down clears it.
            }

            Action::SelectionClear => {
                self.chat.selection_anchor = None;
                self.chat.selection_end = None;
                self.chat.is_selecting = false;
            }

            Action::InputScrollUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let in_edit = self.edit.active();
                    let (buf, cursor) = if in_edit {
                        (self.edit.buffer.clone(), self.edit.cursor)
                    } else {
                        (self.input.buffer.clone(), self.input.cursor)
                    };
                    let wrap = crate::input_wrap::wrap_content(&buf, w, cursor);
                    let new_row = wrap.cursor_row.saturating_sub(1);
                    let new_cursor = crate::input_wrap::byte_offset_at_row_col(
                        &buf,
                        w,
                        new_row,
                        wrap.cursor_col,
                    );
                    if in_edit {
                        self.edit.cursor = new_cursor;
                    } else {
                        self.input.cursor = new_cursor;
                    }
                }
            }

            Action::InputScrollDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let in_edit = self.edit.active();
                    let (buf, cursor) = if in_edit {
                        (self.edit.buffer.clone(), self.edit.cursor)
                    } else {
                        (self.input.buffer.clone(), self.input.cursor)
                    };
                    let wrap = crate::input_wrap::wrap_content(&buf, w, cursor);
                    let max_row = wrap.lines.len().saturating_sub(1);
                    let new_row = (wrap.cursor_row + 1).min(max_row);
                    let new_cursor = crate::input_wrap::byte_offset_at_row_col(
                        &buf,
                        w,
                        new_row,
                        wrap.cursor_col,
                    );
                    if in_edit {
                        self.edit.cursor = new_cursor;
                    } else {
                        self.input.cursor = new_cursor;
                    }
                }
            }

            Action::NvimScrollUp => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-y>").await;
                }
            }

            Action::NvimScrollDown => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-e>").await;
                }
            }

            _ => {}
        }
        false
    }

    // ── Slash command completion ──────────────────────────────────────────────

    fn command_line_at_cursor(&self) -> (usize, String) {
        let before_cursor = &self.input.buffer[..self.input.cursor];
        let start = before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
        (
            start,
            self.input.buffer[start..self.input.cursor].to_string(),
        )
    }

    pub(crate) fn should_show_completion(&self) -> bool {
        let (_, line) = self.command_line_at_cursor();
        line.starts_with('/')
            || self.input.buffer.starts_with('/')
            || self.at_mention_prefix().is_some()
    }

    pub(crate) fn update_completion_overlay(&mut self) {
        // ── @mention completions ──────────────────────────────────────────────
        // Check if the cursor is immediately following an `@` prefix in the
        // input buffer.  If so, show teammate names as completions instead of
        // the normal command completions.
        if let Some(mention_prefix) = self.at_mention_prefix() {
            let items = self.mention_completion_items(&mention_prefix);
            if !items.is_empty() {
                let prev_selected = self.ui.completion.as_ref().map(|o| o.selected).unwrap_or(0);
                let mut overlay = CompletionOverlay::new(items);
                overlay.selected = prev_selected.min(overlay.items.len().saturating_sub(1));
                overlay.adjust_scroll_pub();
                self.ui.completion = Some(overlay);
                return;
            }
        }

        let (_, cmd_line) = self.command_line_at_cursor();
        let parse_source = if cmd_line.starts_with('/') {
            cmd_line
        } else {
            self.input.buffer.clone()
        };
        let parsed = parse(&parse_source);
        let ctx = CommandContext {
            config: self.config.clone(),
            current_model_provider: self.session.model_cfg.provider.clone(),
            current_model_name: self.session.model_cfg.name.clone(),
        };
        let items = self.completion_manager.get_completions(&parsed, &ctx);
        if items.is_empty() {
            self.ui.completion = None;
        } else {
            let prev_selected = self.ui.completion.as_ref().map(|o| o.selected).unwrap_or(0);
            let mut overlay = CompletionOverlay::new(items);
            overlay.selected = prev_selected.min(overlay.items.len().saturating_sub(1));
            overlay.adjust_scroll_pub();
            self.ui.completion = Some(overlay);
        }
    }

    /// Return the `@mention` prefix at the cursor, or `None` if the cursor is
    /// not inside an `@word` token.
    ///
    /// Examples:
    /// - Buffer `"hey @ali"`, cursor=8 → `Some("ali")`
    /// - Buffer `"hey @"`, cursor=5  → `Some("")`
    /// - Buffer `"hello world"`, cursor=11 → `None`
    fn at_mention_prefix(&self) -> Option<String> {
        let buf = &self.input.buffer;
        let cursor = self.input.cursor.min(buf.len());
        let before_cursor = &buf[..cursor];
        // Find the last `@` that is either at the start or preceded by whitespace.
        let at_pos = before_cursor.rfind('@')?;
        let before_at = &before_cursor[..at_pos];
        if !before_at.is_empty() && !before_at.ends_with(|c: char| c.is_whitespace()) {
            return None; // `@` is inside a word, not a mention sigil
        }
        // The text between `@` and the cursor is the partial name.
        let partial = &before_cursor[at_pos + 1..];
        // Must not contain whitespace — a whitespace terminates the mention token.
        if partial.contains(|c: char| c.is_whitespace()) {
            return None;
        }
        Some(partial.to_string())
    }

    /// Build completion items for the `@mention` autocomplete, filtering by
    /// the partial teammate name already typed.
    fn mention_completion_items(&self, partial: &str) -> Vec<CompletionItem> {
        self.ui
            .team_picker_entries
            .iter()
            .filter(|e| !e.is_local) // don't suggest yourself
            .filter(|e| {
                partial.is_empty() || e.name.to_lowercase().starts_with(&partial.to_lowercase())
            })
            .map(|e| CompletionItem {
                display: format!("@{}  [{}]", e.name, e.role),
                value: e.name.clone(),
                description: Some(e.current_task.clone().unwrap_or_else(|| "idle".to_string())),
                score: 0,
            })
            .collect()
    }

    pub(crate) fn apply_completion(&mut self, item: &CompletionItem) {
        let (cmd_start, cmd_line) = self.command_line_at_cursor();
        let is_multiline_cmd = cmd_line.starts_with('/') && cmd_start > 0;
        let parse_source = if is_multiline_cmd {
            cmd_line
        } else {
            self.input.buffer.clone()
        };

        let parsed = parse(&parse_source);
        let new_cmd = match parsed {
            ParsedCommand::PartialCommand { .. } => {
                format!("/{} ", item.value.trim_start_matches('/'))
            }
            ParsedCommand::CompletingArgs {
                command,
                arg_index,
                partial: _,
            } => {
                let prefix = if arg_index == 0 {
                    format!("/{} ", command)
                } else {
                    let body = parse_source.trim_end();
                    let base = body.rfind(' ').map(|i| &body[..=i]).unwrap_or(body);
                    base.to_string()
                };
                format!("{}{} ", prefix, item.value)
            }
            _ => return,
        };

        if is_multiline_cmd {
            let after_cursor = self.input.buffer[self.input.cursor..].to_string();
            let before_cmd = self.input.buffer[..cmd_start].to_string();
            self.input.buffer = format!("{}{}{}", before_cmd, new_cmd, after_cursor);
            self.input.cursor = cmd_start + new_cmd.len();
        } else {
            self.input.buffer = new_cmd;
            self.input.cursor = self.input.buffer.len();
        }
        self.update_completion_overlay();
    }

    // ── Edit-buffer helpers ───────────────────────────────────────────────────

    pub(crate) fn update_editing_segment_live(&mut self) {
        if let Some(idx) = self.edit.message_index {
            let new_text = self.edit.buffer.clone();
            if let Some(ChatSegment::Message(m)) = self.chat.segments.get_mut(idx) {
                match (&m.role, &mut m.content) {
                    (Role::User, MessageContent::Text(t)) => *t = new_text,
                    (Role::Assistant, MessageContent::Text(t)) => *t = new_text,
                    _ => {}
                }
            }
            self.build_display_from_segments();
            self.ui.search.update_matches(&self.chat.lines);
        }
    }

    pub(crate) fn apply_input_to_edit(&self, action: &Action) -> Option<(String, usize)> {
        let (buf, cur) = (&self.edit.buffer, self.edit.cursor);
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
            Action::InputMoveLineUp => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row > 0 {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf,
                            w,
                            ws.cursor_row - 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.layout.input_inner_width as usize;
                if w > 0 {
                    let ws = crate::input_wrap::wrap_content(&buf, w, cur);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        cur = crate::input_wrap::byte_offset_at_row_col(
                            &buf,
                            w,
                            ws.cursor_row + 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
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

// ── Character and word boundary helpers ──────────────────────────────────────

pub(crate) fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

pub(crate) fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[..pos];
    let trimmed = bytes
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[..trimmed]
        .iter()
        .rposition(|&b| b == b' ')
        .map(|i| i + 1)
        .unwrap_or(0)
}

pub(crate) fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[pos..];
    let start = bytes.iter().position(|&b| b != b' ').unwrap_or(0);
    let end = bytes[start..]
        .iter()
        .position(|&b| b == b' ')
        .unwrap_or(bytes.len() - start);
    pos + start + end
}
