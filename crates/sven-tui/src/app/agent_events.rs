// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Agent event and question-request handlers.

use sven_core::AgentEvent;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::QuestionRequest;

use crate::{
    app::{App, FocusPane},
    chat::{
        markdown::format_todos_markdown,
        segment::ChatSegment,
    },
    overlay::question::QuestionModal,
};

impl App {
    // ── Agent event handler ───────────────────────────────────────────────────

    pub(crate) async fn handle_agent_event(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::TextDelta(delta) => {
                self.streaming_is_thinking = false;
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::TextComplete(full_text) => {
                self.chat_segments.push(ChatSegment::Message(Message::assistant(&full_text)));
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
            }
            AgentEvent::ToolCallStarted(tc) => {
                self.tool_args_cache.insert(tc.id.clone(), tc.name.clone());
                self.current_tool = Some(tc.name.clone());
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Message(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                }));
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallFinished { call_id, output, .. } => {
                self.current_tool = None;
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Message(
                    Message::tool_result(&call_id, &output),
                ));
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
                self.chat_segments.push(ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                });
                self.rerender_chat().await;
            }
            AgentEvent::TokenUsage { input, output, cache_read, .. } => {
                let max = 128_000u32;
                self.context_pct = ((input + output) * 100 / max.max(1)).min(100) as u8;
                // Cache hit rate = cached / total_input * 100
                self.cache_hit_pct = if input > 0 && cache_read > 0 {
                    (cache_read * 100 / input).min(100) as u8
                } else {
                    0
                };
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                self.current_tool = None;
                // Clear per-turn cache indicator so it only shows when the
                // provider actively reports cache hits for the current turn.
                self.cache_hit_pct = 0;
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
                self.save_history_async();
                // Only dequeue the next message if no queue item is being edited.
                if self.editing_queue_index.is_none() {
                    if let Some(next) = self.queued.pop_front() {
                        // Shift the selection down by one since the front was removed.
                        self.queue_selected = self.queue_selected
                            .map(|s| s.saturating_sub(1))
                            .filter(|_| !self.queued.is_empty());
                        // If queue is now empty and we were focused on it, return to Input.
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
            AgentEvent::Error(msg) => {
                self.chat_segments.push(ChatSegment::Error(msg.clone()));
                self.rerender_chat().await;
                self.agent_busy = false;
                self.current_tool = None;
            }
            AgentEvent::TodoUpdate(todos) => {
                let todo_md = format_todos_markdown(&todos);
                self.chat_segments.push(ChatSegment::Message(Message::assistant(&todo_md)));
                self.rerender_chat().await;
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.refresh_todo_display().await {
                        tracing::warn!("Failed to refresh todo display: {}", e);
                    }
                }
            }
            AgentEvent::ThinkingDelta(delta) => {
                self.streaming_is_thinking = true;
                self.streaming_assistant_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
            AgentEvent::ThinkingComplete(content) => {
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                let seg_idx = self.chat_segments.len();
                self.chat_segments.push(ChatSegment::Thinking { content });
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
            }
            _ => {}
        }
        false
    }

    // ── Question request handler ──────────────────────────────────────────────

    pub(crate) fn handle_question_request(&mut self, req: QuestionRequest) {
        tracing::debug!(id = %req.id, count = req.questions.len(), "question request received");
        self.question_modal = Some(QuestionModal::new(req.questions, req.answer_tx));
        self.focus = FocusPane::Input;
    }
}
