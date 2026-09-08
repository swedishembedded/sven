// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat-pane interaction keybindings and mouse handling: segment
//! highlight/edit/copy/rerun/delete, scrolling (both the ratatui chat view
//! and the embedded Neovim buffer), search, text selection, and the Esc key's
//! edit-cancel / agent-abort / clear-input priority chain.

use sven_model::{MessageContent, Role};

use crate::{
    app::{App, FocusPane},
    chat::{
        markdown::parse_markdown_to_messages,
        segment::{messages_for_resubmit, segment_at_line, segment_tool_call_id, ChatSegment},
    },
    keys::Action,
    overlay::confirm::ConfirmModal,
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
    /// Dispatch the chat-pane subset of [`Action`]: segment operations,
    /// scrolling, search, mouse selection, and Esc's priority chain.
    pub(crate) async fn dispatch_chat(&mut self, action: Action) -> bool {
        match action {
            Action::EditMessageAtCursor => {
                if let Some(seg_idx) = self.chat.focused_segment {
                    if let Some(text) =
                        crate::chat::segment::segment_editable_text(&self.chat.segments, seg_idx)
                    {
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
                        let qm = crate::app::QueuedMessage {
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
                            self.chat.segments.push(ChatSegment::Message(
                                sven_model::Message::user(&new_content),
                            ));
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
            //   2. The agent is generating → abort the turn (same as Ctrl+C /
            //      `/abort`). Reflexive-Esc-to-abort takes priority over
            //      clearing the input box, since a busy agent means the box
            //      is very likely irrelevant to what the user wants right now.
            //   3. Input box has content / attachments → clear it.
            //   4. Already empty → do nothing.
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
                // No active edit: if the agent is generating, Esc aborts the
                // turn instead of clearing the box - see priority note above.
                if self.agent.busy {
                    self.interrupt_agent().await;
                    return false;
                }
                // Otherwise: clear the input box completely.
                self.input.buffer.clear();
                self.input.cursor = 0;
                self.input.scroll_offset = 0;
                self.input.attachments.clear();
                self.input.history_idx = None;
                self.input.history_draft = None;
                self.ui.completion = None;
            }

            Action::EditMessageCancel => {
                // Cancel queue-item edit - restore original text if available.
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
                                crate::app::QueuedMessage::plain(new_user_content),
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
            Action::ScrollFullPageUp => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-b>").await;
                } else {
                    self.scroll_up(self.layout.chat_height.saturating_sub(1).max(1));
                }
            }
            Action::ScrollFullPageDown => {
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.send_input("<C-f>").await;
                } else {
                    self.scroll_down(self.layout.chat_height.saturating_sub(1).max(1));
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

            // ── Mouse-originated actions ──────────────────────────────────────
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
}
