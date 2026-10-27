// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Drawing the whole TUI into a ratatui [`Frame`], plus the chat-list/peers
//! scroll-offset helpers that keep click handling (`dispatch.rs`) and
//! rendering agreeing on which visual row maps to which item.

use ratatui::{layout::Rect, Frame};

use crate::{
    app::{App, FocusPane},
    markdown::StyledLines,
    ui::{
        input_cursor_screen_pos, nvim_cursor_screen_pos, open_pane_block, ChatPane, CompletionMenu,
        ConfirmModalView, HelpOverlay, InputEditMode, InputPane, QuestionModalView, QueueItem,
        QueuePanel, SearchBar, StatusBar, ToastStack, WelcomeScreen, WhichKeyOverlay,
    },
};

impl App {
    /// What the agent is doing, for the status bar: the running tool, the
    /// model request it is waiting on, its thinking, or a passing notice.
    pub(crate) fn activity_label(&self) -> Option<&str> {
        self.agent
            .current_tool
            .as_deref()
            .or(self.agent.model_waiting.as_deref())
            .or(self.chat.streaming_is_thinking.then_some("Thinking"))
            .or(self.agent.notice.as_deref())
    }

    /// Render the entire TUI into `frame`.
    ///
    /// `nvim_lines`      - rendered Neovim grid lines (empty when no nvim).
    /// `nvim_draw_scroll`- scroll offset used when drawing nvim lines.
    /// `nvim_cursor`     - Neovim cursor position (row, col) in grid space.
    pub(crate) fn view(
        &mut self,
        frame: &mut Frame,
        nvim_lines: &StyledLines,
        nvim_draw_scroll: usize,
        nvim_cursor: Option<(u16, u16)>,
    ) {
        let ascii = self.ascii();
        // ── Full-screen inspector overlay (early return) ──────────────────────
        if let Some(inspector) = &mut self.ui.inspector {
            inspector.pager.render(
                frame,
                &self.ui.search.matches,
                self.ui.search.current,
                &self.ui.search.query,
                self.ui.search.regex.as_ref(),
                ascii,
            );
            if self.ui.search.active {
                let area = frame.area();
                let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                frame.render_widget(
                    SearchBar {
                        query: &self.ui.search.query,
                        match_count: self.ui.search.matches.len(),
                        current_match: self.ui.search.current,
                    },
                    search_area,
                );
            }
            return;
        }
        // ── Full-screen pager (early return) ──────────────────────────────────
        if let Some(pager) = &mut self.ui.pager {
            pager.render(
                frame,
                &self.ui.search.matches,
                self.ui.search.current,
                &self.ui.search.query,
                self.ui.search.regex.as_ref(),
                ascii,
            );
            if self.ui.search.active {
                let area = frame.area();
                let search_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
                frame.render_widget(
                    SearchBar {
                        query: &self.ui.search.query,
                        match_count: self.ui.search.matches.len(),
                        current_match: self.ui.search.current,
                    },
                    search_area,
                );
            }
            return;
        }
        // ── Full-screen session picker (early return) ──────────────────────────
        if self.ui.show_session_picker {
            frame.render_widget(
                crate::ui::SessionPickerOverlay {
                    entries: &self.ui.session_picker_entries,
                    state: &mut self.ui.session_picker_state,
                    ascii,
                },
                frame.area(),
            );
            return;
        }

        let layout = self.layout_for(frame.area());
        // Clean up expired toasts every frame.
        self.ui.prune_toasts();

        // ── Status bar ────────────────────────────────────────────────────────
        let status_model_name: &str = &self.session.model_display;
        let in_edit = self.edit.active();

        frame.render_widget(
            StatusBar {
                model_name: status_model_name,
                mode: self.session.mode,
                context_pct: self.agent.context_pct,
                total_context_pct: self.agent.total_context_pct,
                total_context_tokens: self.agent.total_context_tokens,
                total_output_tokens: self.agent.total_output_tokens,
                total_cost_usd: self
                    .sessions
                    .total_cost_including_children(&self.sessions.active_id),
                cache_hit_pct: self.agent.cache_hit_pct,
                agent_busy: self.agent.busy,
                current_tool: self.activity_label(),
                ascii,
                focus: self.ui.focus,
                spinner_frame: self.agent.spinner_frame,
                streaming_tokens: self.agent.streaming_tokens,
                in_edit,
                in_search: self.ui.search.active,
            },
            layout.status_bar,
        );

        // ── Chat pane ─────────────────────────────────────────────────────────
        // Show the welcome screen when the chat is empty and the agent is idle.
        let show_welcome = self.chat.segments().is_empty()
            && self.chat.streaming_buffer.is_empty()
            && !self.agent.busy
            && self.nvim.disabled;

        if show_welcome {
            let mode_label = self.session.mode.to_string();
            let mode_style = crate::ui::theme::mode_style(self.session.mode);
            frame.render_widget(
                WelcomeScreen {
                    model_name: &self.session.model_display,
                    mode_label: &mode_label,
                    mode_style,
                },
                layout.chat_pane,
            );
        }

        let lines_to_draw = if !nvim_lines.is_empty() {
            nvim_lines
        } else {
            &self.chat.lines
        };
        let editing_range = self
            .edit
            .message_index
            .and_then(|idx| self.chat.segment_line_ranges.get(idx))
            .copied();

        let auto_scroll_paused = !self.chat.auto_scroll && !self.chat.lines.is_empty();
        let highlight_line_range = (self.ui.focus == FocusPane::Chat && self.nvim.disabled)
            .then(|| self.focused_item_range())
            .flatten();
        if !show_welcome {
            frame.render_widget(
                ChatPane {
                    lines: lines_to_draw,
                    scroll_offset: nvim_draw_scroll,
                    focused: self.ui.focus == FocusPane::Chat,
                    ascii,
                    search_query: &self.ui.search.query,
                    search_matches: &self.ui.search.matches,
                    search_current: self.ui.search.current,
                    search_regex: self.ui.search.regex.as_ref(),
                    editing_line_range: editing_range,
                    segment_count: self.chat.segments().len(),
                    auto_scroll_paused,
                    selection: self.chat.normalized_selection(),
                    highlight_line_range,
                },
                layout.chat_pane,
            );
        } // end if !show_welcome

        // Neovim cursor (placed after chat widget renders).
        if let Some(cursor) = nvim_cursor {
            let block_inner = {
                let block = open_pane_block("Chat", self.ui.focus == FocusPane::Chat, ascii);
                block.inner(layout.chat_pane)
            };
            if let Some(pos) = nvim_cursor_screen_pos(
                block_inner,
                cursor,
                nvim_draw_scroll,
                self.ui.focus == FocusPane::Chat,
            ) {
                frame.set_cursor_position(pos);
            }
        }

        // ── Input pane ────────────────────────────────────────────────────────
        let edit_mode = if self.edit.queue_index.is_some() {
            InputEditMode::Queue
        } else if self.edit.message_index.is_some() {
            InputEditMode::Segment
        } else {
            InputEditMode::Normal
        };
        let in_edit = edit_mode != InputEditMode::Normal;
        let (content, cursor_pos, scroll) = if in_edit {
            (
                self.edit.buffer.as_str(),
                self.edit.cursor,
                self.edit.scroll_offset,
            )
        } else {
            (
                self.input.buffer.as_str(),
                self.input.cursor,
                self.input.scroll_offset,
            )
        };

        // Suppress input cursor when a modal owns it.
        let input_cursor_active = (self.ui.focus == FocusPane::Input || in_edit)
            && self.ui.question_modal.is_none()
            && self.ui.confirm_modal.is_none();

        frame.render_widget(
            InputPane {
                content,
                cursor_pos,
                scroll_offset: scroll,
                focused: self.ui.focus == FocusPane::Input || in_edit,
                ascii,
                edit_mode,
                attachments: &self.input.attachments,
                is_resizing: matches!(
                    self.layout.resize_drag,
                    Some(crate::app::layout_cache::ResizeDrag::InputHeight { .. })
                ),
                agent_busy: self.agent.busy,
            },
            layout.input_pane,
        );

        if input_cursor_active {
            if let Some(pos) = input_cursor_screen_pos(
                layout.input_pane,
                content,
                cursor_pos,
                scroll,
                true,
                ascii,
                edit_mode,
                self.input.attachments.len(),
            ) {
                frame.set_cursor_position(pos);
            }
        }

        // ── Activity panel ────────────────────────────────────────────────────
        let agents: Vec<_> = self.activity_agents().into_iter().map(|(_, a)| a).collect();
        if !agents.is_empty() {
            frame.render_widget(
                crate::ui::activity_panel::ActivityPanel {
                    agents: &agents,
                    expanded: self.ui.activity.expanded,
                    selected: self.ui.activity.selected,
                    scroll: self.ui.activity.scroll,
                    focused: self.ui.focus == FocusPane::Activity,
                    ascii,
                    now: std::time::Instant::now(),
                    anim_frame: self.agent.anim_frame,
                },
                layout.activity_pane,
            );
        }

        // ── Queue panel ───────────────────────────────────────────────────────
        if !self.queue.messages.is_empty() {
            let model_labels: Vec<Option<String>> = self
                .queue
                .messages
                .iter()
                .map(|qm| qm.model_transition.as_ref().map(|d| d.display_label()))
                .collect();
            let items: Vec<QueueItem> = self
                .queue
                .messages
                .iter()
                .zip(&model_labels)
                .map(|(qm, label)| QueueItem {
                    content: &qm.content,
                    model_label: label.as_deref(),
                    mode_label: qm.mode_transition,
                })
                .collect();
            frame.render_widget(
                QueuePanel {
                    items: &items,
                    selected: self.queue.selected,
                    editing: self.edit.queue_index,
                    focused: self.ui.focus == FocusPane::Queue,
                    ascii,
                },
                layout.queue_pane,
            );
        }

        // ── Completion overlay ────────────────────────────────────────────────
        if let Some(ref mut overlay) = self.ui.completion {
            frame.render_widget(
                CompletionMenu {
                    overlay,
                    input_pane: layout.input_pane,
                    ascii,
                },
                frame.area(),
            );
        }

        // ── Search bar ────────────────────────────────────────────────────────
        if self.ui.search.active {
            frame.render_widget(
                SearchBar {
                    query: &self.ui.search.query,
                    match_count: self.ui.search.matches.len(),
                    current_match: self.ui.search.current,
                },
                layout.search_bar,
            );
        }

        // ── Help overlay ──────────────────────────────────────────────────────
        if self.ui.show_help {
            frame.render_widget(HelpOverlay { ascii }, frame.area());
        }

        // ── Team picker overlay ───────────────────────────────────────────────
        if self.ui.show_team_picker {
            frame.render_widget(
                crate::ui::TeamPickerOverlay {
                    entries: &self.ui.team_picker_entries,
                    state: &mut self.ui.team_picker_state,
                    ascii,
                },
                frame.area(),
            );
        }

        // ── Question modal ────────────────────────────────────────────────────
        if let Some(modal) = &self.ui.question_modal {
            let result = QuestionModalView {
                questions: &modal.questions,
                current_q: modal.current_q,
                selected_options: &modal.selected_options,
                other_selected: modal.other_selected,
                other_input: &modal.other_input,
                other_cursor: modal.other_cursor,
                focused_option: modal.focused_option,
                ascii,
            }
            .render_with_cursor(frame.area(), frame.buffer_mut());
            if let Some(pos) = result.pos {
                frame.set_cursor_position(pos);
            }
        }

        // ── Confirm modal ─────────────────────────────────────────────────────
        if let Some(modal) = &self.ui.confirm_modal {
            frame.render_widget(
                ConfirmModalView {
                    title: &modal.title,
                    message: &modal.message,
                    confirm_label: &modal.confirm_label,
                    cancel_label: &modal.cancel_label,
                    focused_button: modal.focused_button,
                    has_action: modal.has_action(),
                    ascii,
                    border_color: modal.border_color,
                },
                frame.area(),
            );
        }

        // ── Which-key popup (Ctrl+w chord hint) ──────────────────────────────
        if self.ui.pending_nav {
            frame.render_widget(WhichKeyOverlay { ascii }, frame.area());
        }

        // ── Toast notifications ───────────────────────────────────────────────
        if !self.ui.toasts.is_empty() {
            frame.render_widget(
                ToastStack {
                    toasts: &self.ui.toasts,
                    ascii,
                },
                frame.area(),
            );
        }
    }
}
