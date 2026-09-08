// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Action dispatcher: routes every `Action` variant to `App` state
//! mutations. The raw input-buffer editing keys live in `dispatch_input.rs`
//! and the chat-pane interaction keys (segment ops, scrolling, search,
//! mouse) live in `dispatch_chat.rs`; this file is the top-level router plus
//! everything else - focus/navigation, the message queue, slash-command
//! completion, submit, the team picker, and the chat-list sidebar.

use sven_model::Message;

use crate::{
    app::{App, FocusPane},
    chat::segment::{messages_for_resubmit, ChatSegment},
    commands::{completion::CompletionItem, parse, CommandContext, ParsedCommand},
    keys::Action,
    overlay::completion::CompletionOverlay,
    overlay::confirm::{ConfirmModal, ConfirmedAction},
    pager::PagerOverlay,
};

impl App {
    // ── Action dispatcher ─────────────────────────────────────────────────────

    pub(crate) async fn dispatch(&mut self, action: Action) -> bool {
        // Route input-manipulation actions to the edit buffer whenever we are in
        // any edit mode - both chat-segment edits and queue-item edits.
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

        // Raw input-buffer editing and chat-pane interaction each have their
        // own cohesive dispatcher (see module docs above); every other
        // action is handled directly below.
        match &action {
            Action::InputChar(_)
            | Action::InputNewline
            | Action::InputBackspace
            | Action::InputDelete
            | Action::InputMoveCursorLeft
            | Action::InputMoveCursorRight
            | Action::InputMoveWordLeft
            | Action::InputMoveWordRight
            | Action::InputMoveLineStart
            | Action::InputMoveLineEnd
            | Action::InputMoveLineUp
            | Action::InputMoveLineDown
            | Action::InputPageUp
            | Action::InputPageDown
            | Action::InputDeleteToEnd
            | Action::InputDeleteToStart
            | Action::InputHistoryUp
            | Action::InputHistoryDown
            | Action::ResizeInputGrow
            | Action::ResizeInputShrink
            | Action::InputScrollUp
            | Action::InputScrollDown => return self.dispatch_input(action).await,

            Action::EditMessageAtCursor
            | Action::DeleteChatSegment
            | Action::RemoveChatSegment
            | Action::CopySegment
            | Action::CopyAll
            | Action::RerunFromSegment
            | Action::EditMessageConfirm
            | Action::InputEscape
            | Action::EditMessageCancel
            | Action::SubmitBufferToAgent
            | Action::ChatHighlightDown
            | Action::ChatHighlightUp
            | Action::ShowChatHelp
            | Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollFullPageUp
            | Action::ScrollFullPageDown
            | Action::ScrollTop
            | Action::ScrollBottom
            | Action::SearchOpen
            | Action::SearchClose
            | Action::SearchInput(_)
            | Action::SearchBackspace
            | Action::SearchNextMatch
            | Action::SearchPrevMatch
            | Action::ToggleDelegateSummary
            | Action::ChatScrollbarClick { .. }
            | Action::ChatContentClick { .. }
            | Action::SelectionExtend { .. }
            | Action::SelectionFinish
            | Action::SelectionClear
            | Action::NvimScrollUp
            | Action::NvimScrollDown => return self.dispatch_chat(action).await,

            _ => {}
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
                self.interrupt_agent().await;
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

            // ── Session picker actions (`/resume`) ────────────────────────────
            Action::SessionPickerNext => {
                self.ui.session_picker_next();
            }
            Action::SessionPickerPrev => {
                self.ui.session_picker_prev();
            }
            Action::SessionPickerSelect => {
                let selected = self.ui.session_picker_selected().cloned();
                self.ui.show_session_picker = false;
                if let Some(unified) = selected {
                    let id = self.sessions.ensure_registered(unified);
                    self.switch_session(id).await;
                }
            }
            Action::SessionPickerClose => {
                self.ui.show_session_picker = false;
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
                    // Save the updated status to disk (native ATIF sessions
                    // only; a legacy-only entry is archived in memory and
                    // picks up the status on its next real save, which also
                    // migrates it off the YAML format - see `resolve_session_path`).
                    if let Some(entry) = self.sessions.get(&id) {
                        if let Some(path) = entry.session_path.clone() {
                            if path.exists() {
                                if let Ok(mut trajectory) =
                                    sven_session_store::load_session_from(&path)
                                {
                                    if let Some(mut meta) =
                                        sven_session_store::SvenSessionMeta::from_trajectory(
                                            &trajectory,
                                        )
                                    {
                                        meta.status = sven_session_store::ChatStatus::Archived;
                                        meta.touch();
                                        meta.apply_to_trajectory(&mut trajectory);
                                        let _ = atif::persist::write_trajectory_atomic(
                                            &path,
                                            &trajectory,
                                            None,
                                        );
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
                // Focus the chat list so it accepts input keys (k/j, Enter, etc.)
                // just as when switching via Ctrl+w h / Ctrl+w l.
                self.ui.focus = FocusPane::ChatList;
            }

            Action::QueueClick { index } => {
                // Clear selection (click is outside chat content).
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
                let prev_selected = self
                    .ui
                    .completion
                    .as_ref()
                    .map(|o| o.selected())
                    .unwrap_or(0);
                let mut overlay = CompletionOverlay::new(items);
                overlay.list_state.select(Some(
                    prev_selected.min(overlay.items.len().saturating_sub(1)),
                ));
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
            let prev_selected = self
                .ui
                .completion
                .as_ref()
                .map(|o| o.selected())
                .unwrap_or(0);
            let mut overlay = CompletionOverlay::new(items);
            overlay.list_state.select(Some(
                prev_selected.min(overlay.items.len().saturating_sub(1)),
            ));
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
        // Must not contain whitespace - a whitespace terminates the mention token.
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

    // ── Edit-buffer live preview ──────────────────────────────────────────────

    pub(crate) fn update_editing_segment_live(&mut self) {
        if let Some(idx) = self.edit.message_index {
            let new_text = self.edit.buffer.clone();
            if let Some(ChatSegment::Message(m)) = self.chat.segments.get_mut(idx) {
                match (&m.role, &mut m.content) {
                    (sven_model::Role::User, sven_model::MessageContent::Text(t)) => *t = new_text,
                    (sven_model::Role::Assistant, sven_model::MessageContent::Text(t)) => {
                        *t = new_text
                    }
                    _ => {}
                }
            }
            self.build_display_from_segments();
            self.ui.search.update_matches(&self.chat.lines);
        }
    }
}
