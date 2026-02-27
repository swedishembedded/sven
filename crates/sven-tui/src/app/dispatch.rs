// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Action dispatcher: maps every `Action` variant to `App` state mutations.

use sven_model::{Message, MessageContent, Role};

use crate::{
    app::{App, FocusPane, QueuedMessage},
    chat::{
        markdown::parse_markdown_to_messages,
        segment::{
            messages_for_resubmit, segment_editable_text, segment_tool_call_id,
            ChatSegment,
        },
    },
    commands::{
        completion::CompletionItem,
        parse, CommandContext, ParsedCommand,
    },
    keys::Action,
    overlay::completion::CompletionOverlay,
    pager::PagerOverlay,
};

impl App {
    // ── Action dispatcher ─────────────────────────────────────────────────────

    pub(crate) async fn dispatch(&mut self, action: Action) -> bool {
        // Route input-manipulation actions to the edit buffer whenever we are in
        // any edit mode — both chat-segment edits and queue-item edits.
        if self.editing_message_index.is_some() || self.editing_queue_index.is_some() {
            if let Some((buf, cur)) = self.apply_input_to_edit(&action) {
                self.edit_buffer = buf;
                self.edit_cursor = cur;
                // Live-preview only makes sense for chat segments (not queue items).
                if self.editing_message_index.is_some() {
                    self.update_editing_segment_live();
                    self.rerender_chat().await;
                }
                return false;
            }
        }

        match action {
            Action::FocusInput => {
                self.focus = FocusPane::Input;
            }
            Action::NavUp => {
                // Ctrl+w k: move focus upward through visible panes
                match self.focus {
                    FocusPane::Input => {
                        if !self.queued.is_empty() {
                            if self.queue_selected.is_none() {
                                self.queue_selected = Some(0);
                            }
                            self.focus = FocusPane::Queue;
                        } else {
                            self.focus = FocusPane::Chat;
                        }
                    }
                    FocusPane::Queue => {
                        self.focus = FocusPane::Chat;
                    }
                    FocusPane::Chat => {
                        // Already at the top; stay in Chat
                    }
                }
            }
            Action::NavDown => {
                // Ctrl+w j: move focus downward through visible panes
                match self.focus {
                    FocusPane::Chat => {
                        if !self.queued.is_empty() {
                            if self.queue_selected.is_none() {
                                self.queue_selected = Some(0);
                            }
                            self.focus = FocusPane::Queue;
                        } else {
                            self.focus = FocusPane::Input;
                        }
                    }
                    FocusPane::Queue => {
                        self.focus = FocusPane::Input;
                    }
                    FocusPane::Input => {
                        // Already at the bottom; stay in Input
                    }
                }
            }
            Action::FocusQueue => {
                if !self.queued.is_empty() {
                    if self.queue_selected.is_none() {
                        self.queue_selected = Some(0);
                    }
                    self.focus = FocusPane::Queue;
                }
            }
            Action::QueueNavUp => {
                if let Some(sel) = self.queue_selected {
                    self.queue_selected = Some(sel.saturating_sub(1));
                } else if !self.queued.is_empty() {
                    self.queue_selected = Some(0);
                }
            }
            Action::QueueNavDown => {
                let len = self.queued.len();
                if len > 0 {
                    let sel = self.queue_selected.unwrap_or(0);
                    self.queue_selected = Some((sel + 1).min(len - 1));
                }
            }
            Action::QueueEditSelected => {
                if let Some(idx) = self.queue_selected {
                    if let Some(qm) = self.queued.get(idx) {
                        let text = qm.content.clone();
                        self.editing_queue_index = Some(idx);
                        self.edit_cursor = text.len();
                        self.edit_original_text = Some(text.clone());
                        self.edit_buffer = text;
                        self.focus = FocusPane::Input;
                    }
                }
            }

            Action::EditMessageAtCursor => {
                // Use the keyboard-focused segment (centre of viewport).
                if let Some(seg_idx) = self.focused_chat_segment {
                    if let Some(text) = segment_editable_text(&self.chat_segments, seg_idx) {
                        self.editing_message_index = Some(seg_idx);
                        self.edit_cursor = text.len();
                        self.edit_original_text = Some(text.clone());
                        self.edit_buffer = text;
                        self.focus = FocusPane::Input;
                        self.update_editing_segment_live();
                        self.rerender_chat().await;
                    }
                }
            }

            Action::DeleteChatSegment => {
                // Truncate chat history from the focused segment onward.
                // Only operates on editable (user / assistant text) segments so
                // that the user cannot accidentally wipe non-text entries.
                if let Some(seg_idx) = self.focused_chat_segment {
                    // Cancel any ongoing edit that would be invalidated.
                    if self.editing_message_index.map(|i| i >= seg_idx).unwrap_or(false) {
                        self.editing_message_index = None;
                        self.edit_buffer.clear();
                        self.edit_cursor = 0;
                        self.edit_scroll_offset = 0;
                        self.edit_original_text = None;
                    }
                    self.chat_segments.truncate(seg_idx);
                    self.collapsed_segments.retain(|&i| i < seg_idx);
                    self.focused_chat_segment = None;
                    self.rerender_chat().await;
                    self.save_history_async();
                }
            }

            Action::RemoveChatSegment => {
                // Remove the focused segment from the conversation.  If it is
                // one half of a ToolCall/ToolResult pair the other half is also
                // removed to keep the history consistent for the LLM API.
                if let Some(seg_idx) = self.focused_chat_segment {
                    // Collect indices to remove (sorted descending so removals
                    // don't shift earlier indices).
                    let paired_id: Option<String> = self
                        .chat_segments
                        .get(seg_idx)
                        .and_then(segment_tool_call_id)
                        .map(String::from);

                    let mut to_remove: Vec<usize> = vec![seg_idx];
                    if let Some(ref call_id) = paired_id {
                        // Find the matching counterpart (ToolCall↔ToolResult).
                        for (i, seg) in self.chat_segments.iter().enumerate() {
                            if i != seg_idx {
                                if segment_tool_call_id(seg) == Some(call_id.as_str()) {
                                    to_remove.push(i);
                                }
                            }
                        }
                    }
                    to_remove.sort_unstable_by(|a, b| b.cmp(a)); // descending

                    // Cancel in-progress edit if it targets a removed segment.
                    if self.editing_message_index
                        .map(|i| to_remove.contains(&i))
                        .unwrap_or(false)
                    {
                        self.editing_message_index = None;
                        self.edit_buffer.clear();
                        self.edit_cursor = 0;
                        self.edit_scroll_offset = 0;
                        self.edit_original_text = None;
                    }

                    for idx in &to_remove {
                        if *idx < self.chat_segments.len() {
                            self.chat_segments.remove(*idx);
                        }
                    }

                    // Shift collapsed-segment indices to account for removal.
                    let min_removed = *to_remove.last().unwrap_or(&seg_idx);
                    let removed_count = to_remove.len();
                    self.collapsed_segments = self
                        .collapsed_segments
                        .iter()
                        .filter_map(|&i| {
                            if to_remove.contains(&i) {
                                None
                            } else if i > min_removed {
                                Some(i - removed_count)
                            } else {
                                Some(i)
                            }
                        })
                        .collect();

                    self.focused_chat_segment = None;
                    self.rerender_chat().await;
                    self.save_history_async();
                }
            }

            Action::RerunFromSegment => {
                // Truncate to just before the last user-text message that
                // precedes the focused segment, then re-submit to the agent.
                // This mirrors the `enqueue_or_send_text` flow exactly.
                if let Some(seg_idx) = self.focused_chat_segment {
                    // Find the last user text message strictly before seg_idx.
                    let last_user = (0..seg_idx).rev().find_map(|i| {
                        match self.chat_segments.get(i) {
                            Some(ChatSegment::Message(m)) => {
                                if matches!((&m.role, &m.content),
                                    (sven_model::Role::User, sven_model::MessageContent::Text(_)))
                                {
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
                        }
                    });

                    if let Some((user_idx, user_text)) = last_user {
                        // Cancel any in-progress edit.
                        self.editing_message_index = None;
                        self.edit_buffer.clear();
                        self.edit_cursor = 0;
                        self.edit_scroll_offset = 0;
                        self.edit_original_text = None;

                        // Truncate to just before the user message so that
                        // messages_for_resubmit returns history without it.
                        self.chat_segments.truncate(user_idx);
                        self.collapsed_segments.retain(|&i| i < user_idx);

                        let messages = messages_for_resubmit(&self.chat_segments);

                        // Re-add the user message (agent will add it again via
                        // Resubmit, mirroring the enqueue_or_send_text pattern).
                        self.chat_segments.push(ChatSegment::Message(
                            sven_model::Message::user(&user_text),
                        ));
                        self.focused_chat_segment = None;
                        let qm = QueuedMessage {
                            content: user_text,
                            model_transition: None,
                            mode_transition: None,
                        };
                        self.rerender_chat().await;
                        self.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_resubmit_to_agent(messages, qm).await;
                    }
                }
            }

            Action::DeleteQueuedMessage => {
                if let Some(idx) = self.queue_selected {
                    if idx < self.queued.len() {
                        // If we were editing this item, cancel the edit first.
                        if self.editing_queue_index == Some(idx) {
                            self.editing_queue_index = None;
                            self.edit_buffer.clear();
                            self.edit_cursor = 0;
                            self.edit_scroll_offset = 0;
                            self.edit_original_text = None;
                        }
                        self.queued.remove(idx);
                        // Keep selection in bounds.
                        if self.queued.is_empty() {
                            self.queue_selected = None;
                            if self.focus == FocusPane::Queue {
                                self.focus = FocusPane::Input;
                            }
                        } else {
                            self.queue_selected = Some(idx.min(self.queued.len() - 1));
                        }
                    }
                }
            }
            Action::EditMessageConfirm => {
                // Handle queue-item edit confirm.
                if let Some(q_idx) = self.editing_queue_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_queue_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
                    self.edit_original_text = None;
                    if !new_content.is_empty() {
                        if let Some(entry) = self.queued.get_mut(q_idx) {
                            entry.content = new_content;
                        }
                    }
                    // Return focus to Queue if it still has items, otherwise Input.
                    self.focus = if self.queued.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    // If the agent finished while we were editing, pick up the queue now.
                    self.try_dequeue_next().await;
                    return false;
                }
                // Handle chat-segment edit confirm.
                if let Some(i) = self.editing_message_index {
                    let new_content = self.edit_buffer.trim().to_string();
                    self.editing_message_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
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
                            // Consume any staged model/mode override so that
                            // "/model X" typed before editing takes effect on
                            // this resubmit (same behaviour as sending a new
                            // message via enqueue_or_send_text).
                            let (staged_model, staged_mode) = self.session.consume_staged();
                            let qm = crate::app::QueuedMessage {
                                content: new_content.clone(),
                                model_transition: staged_model
                                    .map(crate::app::ModelDirective::SwitchTo),
                                mode_transition: staged_mode,
                            };
                            self.chat_segments.truncate(i + 1);
                            self.chat_segments.pop();
                            self.chat_segments
                                .push(ChatSegment::Message(Message::user(&new_content)));
                            let messages = messages_for_resubmit(&self.chat_segments);
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(messages, qm).await;
                        }
                        (Role::Assistant, MessageContent::Text(_)) => {
                            if let Some(ChatSegment::Message(m)) = self.chat_segments.get_mut(i) {
                                m.content = MessageContent::Text(new_content);
                            }
                            self.build_display_from_segments();
                            self.search.update_matches(&self.chat_lines);
                            self.rerender_chat().await;
                            self.save_history_async();
                        }
                        _ => {}
                    }
                }
            }
            Action::EditMessageCancel => {
                // Cancel queue-item edit — restore original text if available.
                if self.editing_queue_index.is_some() {
                    if let (Some(q_idx), Some(original)) =
                        (self.editing_queue_index, self.edit_original_text.clone())
                    {
                        if let Some(entry) = self.queued.get_mut(q_idx) {
                            entry.content = original;
                        }
                    }
                    self.editing_queue_index = None;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.edit_scroll_offset = 0;
                    self.edit_original_text = None;
                    // Return focus to Queue if it still has items, otherwise Input.
                    self.focus = if self.queued.is_empty() {
                        FocusPane::Input
                    } else {
                        FocusPane::Queue
                    };
                    // If the agent finished while we were editing, pick up the queue now.
                    self.try_dequeue_next().await;
                    return false;
                }
                // Cancel chat-segment edit.
                if let Some(idx) = self.editing_message_index {
                    if let Some(original) = self.edit_original_text.clone() {
                        match self.chat_segments.get_mut(idx) {
                            Some(ChatSegment::Message(m)) => {
                                match (&m.role, &mut m.content) {
                                    (Role::User, MessageContent::Text(t)) => {
                                        *t = original;
                                    }
                                    (Role::Assistant, MessageContent::Text(t)) => {
                                        *t = original;
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        self.build_display_from_segments();
                        self.search.update_matches(&self.chat_lines);
                    }
                }
                self.editing_message_index = None;
                self.edit_buffer.clear();
                self.edit_cursor = 0;
                self.edit_scroll_offset = 0;
                self.edit_original_text = None;
            }

            Action::SubmitBufferToAgent => {
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

                            // Nvim-buffer slash commands apply state immediately
                            // (no staging); the buffer already represents the full
                            // conversation so there is no "next message" concept.
                            let trimmed = new_user_content.trim();
                            if trimmed.starts_with('/') {
                                return self.submit_nvim_command(trimmed).await;
                            }

                            // Nvim-specific: replace chat segments with the full
                            // buffer content (including the new user message) and
                            // rebuild the tool-call args cache before sending.
                            self.chat_segments = messages
                                .iter()
                                .map(|m| ChatSegment::Message(m.clone()))
                                .collect();
                            self.tool_args_cache.clear();
                            for msg in &messages {
                                if let MessageContent::ToolCall { tool_call_id, function } =
                                    &msg.content
                                {
                                    self.tool_args_cache
                                        .insert(tool_call_id.clone(), function.name.clone());
                                }
                            }
                            self.rerender_chat().await;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(
                                messages,
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
                    self.auto_scroll = false;
                }
            }
            Action::ScrollBottom => {
                self.auto_scroll = true;
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
                    self.search.current = (self.search.current + 1) % self.search.matches.len();
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
                // Auto-trigger / update completion overlay when on a slash-command line.
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.completion_overlay = None;
                }
            }
            Action::InputNewline => {
                self.input_buffer.insert(self.input_cursor, '\n');
                self.input_cursor += 1;
                // Dismiss the completion overlay when starting a new line.
                self.completion_overlay = None;
            }
            Action::InputBackspace => {
                if self.input_cursor > 0 {
                    let prev = prev_char_boundary(&self.input_buffer, self.input_cursor);
                    self.input_buffer.remove(prev);
                    self.input_cursor = prev;
                }
                if self.should_show_completion() {
                    self.update_completion_overlay();
                } else {
                    self.completion_overlay = None;
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
            Action::InputMoveLineUp => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input_buffer, w, self.input_cursor);
                    if ws.cursor_row > 0 {
                        self.input_cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input_buffer,
                            w,
                            ws.cursor_row - 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputMoveLineDown => {
                let w = self.last_input_inner_width as usize;
                if w > 0 {
                    let ws =
                        crate::input_wrap::wrap_content(&self.input_buffer, w, self.input_cursor);
                    if ws.cursor_row + 1 < ws.lines.len() {
                        self.input_cursor = crate::input_wrap::byte_offset_at_row_col(
                            &self.input_buffer,
                            w,
                            ws.cursor_row + 1,
                            ws.cursor_col,
                        );
                    }
                }
            }
            Action::InputPageUp => {
                let h = self.last_input_inner_height as usize;
                if self.editing_message_index.is_some() || self.editing_queue_index.is_some() {
                    self.edit_scroll_offset = self.edit_scroll_offset.saturating_sub(h);
                } else {
                    self.input_scroll_offset = self.input_scroll_offset.saturating_sub(h);
                }
            }
            Action::InputPageDown => {
                let w = self.last_input_inner_width as usize;
                let h = self.last_input_inner_height as usize;
                if w > 0 && h > 0 {
                    let in_edit = self.editing_message_index.is_some()
                        || self.editing_queue_index.is_some();
                    let content =
                        if in_edit { &self.edit_buffer } else { &self.input_buffer };
                    let ws = crate::input_wrap::wrap_content(content, w, 0);
                    let max = ws.lines.len().saturating_sub(h);
                    if in_edit {
                        self.edit_scroll_offset = (self.edit_scroll_offset + h).min(max);
                    } else {
                        self.input_scroll_offset = (self.input_scroll_offset + h).min(max);
                    }
                }
            }
            Action::InputDeleteToEnd => self.input_buffer.truncate(self.input_cursor),
            Action::InputDeleteToStart => {
                self.input_buffer = self.input_buffer[self.input_cursor..].to_string();
                self.input_cursor = 0;
            }

            Action::Submit => {
                self.completion_overlay = None;
                let text = std::mem::take(&mut self.input_buffer).trim().to_string();
                self.input_cursor = 0;
                self.input_scroll_offset = 0;
                if text.is_empty() {
                    return false;
                }
                return self.submit_user_input(&text).await;
            }

            Action::CompletionNext => {
                if let Some(overlay) = &mut self.completion_overlay {
                    overlay.select_next();
                } else if self.should_show_completion() {
                    self.update_completion_overlay();
                }
            }

            Action::CompletionPrev => {
                if let Some(overlay) = &mut self.completion_overlay {
                    overlay.select_prev();
                }
            }

            Action::CompletionSelect => {
                if let Some(overlay) = self.completion_overlay.take() {
                    if let Some(item) = overlay.selected_item() {
                        let item = item.clone();
                        self.apply_completion(&item);
                    }
                }
            }

            Action::CompletionCancel => {
                self.completion_overlay = None;
            }

            Action::InterruptAgent => {
                if self.agent_busy {
                    // Set abort_pending so the queue does not auto-advance after
                    // the run stops.
                    self.abort_pending = true;
                    self.send_abort_signal().await;
                }
            }

            Action::ForceSubmitQueuedMessage => {
                if let Some(idx) = self.queue_selected {
                    self.force_submit_queued_message(idx).await;
                }
            }

            Action::QueueSubmitSelected => {
                if let Some(idx) = self.queue_selected {
                    if !self.agent_busy && idx < self.queued.len() {
                        // Clear abort_pending: user is manually resuming the queue.
                        self.abort_pending = false;
                        if let Some(qm) = self.queued.remove(idx) {
                            self.queue_selected = if self.queued.is_empty() {
                                None
                            } else {
                                Some(idx.min(self.queued.len() - 1))
                            };
                            if self.queued.is_empty() && self.focus == FocusPane::Queue {
                                self.focus = FocusPane::Input;
                            }
                            let history = messages_for_resubmit(&self.chat_segments);
                            self.chat_segments.push(ChatSegment::Message(
                                Message::user(&qm.content),
                            ));
                            self.save_history_async();
                            self.rerender_chat().await;
                            self.auto_scroll = true;
                            self.scroll_to_bottom();
                            self.send_resubmit_to_agent(history, qm).await;
                        }
                    }
                }
            }

            Action::CycleMode => {
                self.session.cycle_mode();
            }

            Action::Help => {
                self.show_help = !self.show_help;
            }

            Action::OpenPager => {
                let mut pager = PagerOverlay::new(self.chat_lines.clone());
                if let Some(line) = self.search.current_line() {
                    pager.scroll_to_line(line);
                }
                self.pager = Some(pager);
            }

            _ => {}
        }
        false
    }

    // ── Slash command completion ──────────────────────────────────────────────

    /// Return `(command_line_start_byte, command_line_as_owned_string)` for the
    /// line containing the cursor.  The "command line" is the text from the last
    /// `\n` before the cursor position to the cursor.
    ///
    /// Returns owned data so callers can freely mutate `self` afterwards.
    fn command_line_at_cursor(&self) -> (usize, String) {
        let before_cursor = &self.input_buffer[..self.input_cursor];
        let start = before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
        (start, self.input_buffer[start..self.input_cursor].to_string())
    }

    /// Return true when the input should trigger the completion overlay.
    ///
    /// True when either the whole buffer starts with `/` (single-line mode) or
    /// when the current line at the cursor starts with `/` (multi-line mode).
    fn should_show_completion(&self) -> bool {
        let (_, line) = self.command_line_at_cursor();
        line.starts_with('/') || self.input_buffer.starts_with('/')
    }

    /// Regenerate completions from the current `input_buffer` and update (or
    /// dismiss) the `completion_overlay`.
    ///
    /// In multi-line mode the completion is driven by the line containing the
    /// cursor so that `/command` patterns typed in the middle of a message still
    /// get completions.
    pub(crate) fn update_completion_overlay(&mut self) {
        let (_, cmd_line) = self.command_line_at_cursor();
        let parse_source = if cmd_line.starts_with('/') {
            cmd_line
        } else {
            self.input_buffer.clone()
        };
        let parsed = parse(&parse_source);
        let ctx = CommandContext {
            config: self.config.clone(),
            current_model_provider: self.session.model_cfg.provider.clone(),
            current_model_name: self.session.model_cfg.name.clone(),
        };
        let items = self.completion_manager.get_completions(&parsed, &ctx);
        if items.is_empty() {
            self.completion_overlay = None;
        } else {
            let prev_selected =
                self.completion_overlay.as_ref().map(|o| o.selected).unwrap_or(0);
            let mut overlay = CompletionOverlay::new(items);
            overlay.selected = prev_selected.min(overlay.items.len().saturating_sub(1));
            overlay.adjust_scroll_pub();
            self.completion_overlay = Some(overlay);
        }
    }

    /// Apply the selected completion item to `input_buffer`.
    ///
    /// In single-line mode (buffer starts with `/`), the entire buffer is
    /// replaced.  In multi-line mode, only the command-line portion of the
    /// current line is updated.
    pub(crate) fn apply_completion(&mut self, item: &CompletionItem) {
        let (cmd_start, cmd_line) = self.command_line_at_cursor();
        let is_multiline_cmd = cmd_line.starts_with('/') && cmd_start > 0;
        let parse_source = if is_multiline_cmd { cmd_line } else { self.input_buffer.clone() };

        let parsed = parse(&parse_source);
        let new_cmd = match parsed {
            ParsedCommand::PartialCommand { .. } => {
                format!("/{} ", item.value.trim_start_matches('/'))
            }
            ParsedCommand::CompletingArgs { command, arg_index, partial: _ } => {
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
            // Replace only the command portion of the current line.
            // Capture remaining text after the cursor before mutation.
            let after_cursor = self.input_buffer[self.input_cursor..].to_string();
            let before_cmd = self.input_buffer[..cmd_start].to_string();
            self.input_buffer = format!("{}{}{}", before_cmd, new_cmd, after_cursor);
            self.input_cursor = cmd_start + new_cmd.len();
        } else {
            self.input_buffer = new_cmd;
            self.input_cursor = self.input_buffer.len();
        }
        self.update_completion_overlay();
    }

    // ── Edit-buffer helpers ───────────────────────────────────────────────────

    /// Update the segment being edited with the current `edit_buffer` content
    /// (live preview while the user types).
    pub(crate) fn update_editing_segment_live(&mut self) {
        if let Some(idx) = self.editing_message_index {
            let new_text = self.edit_buffer.clone();
            match self.chat_segments.get_mut(idx) {
                Some(ChatSegment::Message(m)) => {
                    match (&m.role, &mut m.content) {
                        (Role::User, MessageContent::Text(t)) => {
                            *t = new_text;
                        }
                        (Role::Assistant, MessageContent::Text(t)) => {
                            *t = new_text;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            self.build_display_from_segments();
            self.search.update_matches(&self.chat_lines);
        }
    }

    /// Apply an `Input*` action to the current `(edit_buffer, edit_cursor)`.
    ///
    /// Returns `Some((new_buf, new_cur))` when the action was consumed by the
    /// edit mode; returns `None` for non-input actions.
    pub(crate) fn apply_input_to_edit(&self, action: &Action) -> Option<(String, usize)> {
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
            Action::InputMoveCursorLeft  => cur = prev_char_boundary(&buf, cur),
            Action::InputMoveCursorRight => {
                if cur < buf.len() {
                    let ch = buf[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    cur += ch;
                }
            }
            Action::InputMoveWordLeft  => cur = prev_word_boundary(&buf, cur),
            Action::InputMoveWordRight => cur = next_word_boundary(&buf, cur),
            Action::InputMoveLineStart => cur = 0,
            Action::InputMoveLineEnd   => cur = buf.len(),
            Action::InputMoveLineUp => {
                let w = self.last_input_inner_width as usize;
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
                let w = self.last_input_inner_width as usize;
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
            Action::InputDeleteToEnd   => buf.truncate(cur),
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
    let bytes   = &s.as_bytes()[..pos];
    let trimmed = bytes.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    bytes[..trimmed].iter().rposition(|&b| b == b' ').map(|i| i + 1).unwrap_or(0)
}

pub(crate) fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = &s.as_bytes()[pos..];
    let start = bytes.iter().position(|&b| b != b' ').unwrap_or(0);
    let end   = bytes[start..].iter().position(|&b| b == b' ').unwrap_or(bytes.len() - start);
    pos + start + end
}
