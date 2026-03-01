// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent event and question-request handlers.

use sven_core::AgentEvent;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::QuestionRequest;

use crate::{
    app::{App, FocusPane},
    chat::{
        markdown::format_todos_markdown,
        segment::{messages_for_resubmit, ChatSegment},
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
                self.chat_segments
                    .push(ChatSegment::Message(Message::assistant(&full_text)));
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                self.save_history_async();
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
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ToolCallFinished {
                call_id, output, ..
            } => {
                self.current_tool = None;
                let seg_idx = self.chat_segments.len();
                self.chat_segments
                    .push(ChatSegment::Message(Message::tool_result(
                        &call_id, &output,
                    )));
                if self.no_nvim {
                    self.collapsed_segments.insert(seg_idx);
                }
                // Signal the run-loop to check and restore terminal state.
                // Subprocesses spawned by tools may alter raw-mode or other
                // terminal settings; the recovery runs before the next draw.
                self.needs_terminal_recover = true;
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
            }
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            } => {
                self.chat_segments.push(ChatSegment::ContextCompacted {
                    tokens_before,
                    tokens_after,
                    strategy,
                    turn,
                });
                self.save_history_async();
                self.rerender_chat().await;
            }
            AgentEvent::TokenUsage {
                input,
                cache_read,
                cache_write,
                max_tokens,
                ..
            } => {
                // Providers like Anthropic send two Usage events per turn:
                //   1. message_start  – input + cache stats, output = 0
                //   2. message_delta  – output count only, input = cache = 0
                // Only update context/cache display when we have input stats
                // (event 1).  Ignoring event 2 prevents the second event from
                // wiping out the valid numbers we just set.
                if input > 0 || cache_read > 0 || cache_write > 0 {
                    // Total tokens in the context = processed + from cache + written to cache.
                    let total_ctx = input + cache_read + cache_write;
                    let max = if max_tokens > 0 {
                        max_tokens as u32
                    } else {
                        200_000
                    };
                    self.context_pct = (total_ctx * 100 / max).min(100) as u8;
                    // Cache hit rate = tokens served from cache / total context tokens.
                    self.cache_hit_pct = if total_ctx > 0 && cache_read > 0 {
                        (cache_read * 100 / total_ctx).min(100) as u8
                    } else {
                        0
                    };
                }
            }
            AgentEvent::TurnComplete => {
                self.agent_busy = false;
                self.current_tool = None;
                // Keep context_pct and cache_hit_pct from the last turn so
                // the status bar continues to show useful stats between turns.
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
                self.save_history_async();
                // Only dequeue the next message if no queue item is being edited
                // and an abort did not explicitly suppress auto-advance.
                if self.editing_queue_index.is_none() && !self.abort_pending {
                    if let Some(next) = self.queued.pop_front() {
                        // Shift the selection down by one since the front was removed.
                        self.queue_selected = self
                            .queue_selected
                            .map(|s| s.saturating_sub(1))
                            .filter(|_| !self.queued.is_empty());
                        // If queue is now empty and we were focused on it, return to Input.
                        if self.queued.is_empty() && self.focus == FocusPane::Queue {
                            self.focus = FocusPane::Input;
                        }
                        self.chat_segments
                            .push(ChatSegment::Message(Message::user(&next.content)));
                        self.save_history_async();
                        self.rerender_chat().await;
                        self.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_to_agent(next).await;
                    }
                }
            }
            AgentEvent::Aborted { partial_text } => {
                // The agent committed `partial_text` to its own session before
                // emitting this event.  The TUI streaming buffer already shows
                // the partial text on screen; commit it to chat_segments so the
                // history and display stay in sync.
                self.streaming_assistant_buffer.clear();
                self.streaming_is_thinking = false;
                if !partial_text.is_empty() {
                    self.chat_segments
                        .push(ChatSegment::Message(Message::assistant(&partial_text)));
                }
                self.agent_busy = false;
                self.current_tool = None;
                if let Some(nvim_bridge) = &self.nvim_bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.set_modifiable(true).await;
                }
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.pager {
                    pager.set_lines(self.chat_lines.clone());
                }
                // If abort_pending is set, the user did a plain /abort — keep
                // the queue as-is and wait for manual submit.
                // If abort_pending is false (force-submit path), auto-dequeue
                // the front of the queue as a full Resubmit so the model sees
                // the complete history including any committed partial text.
                if !self.abort_pending && self.editing_queue_index.is_none() {
                    if let Some(next) = self.queued.pop_front() {
                        self.queue_selected = self
                            .queue_selected
                            .map(|s| s.saturating_sub(1))
                            .filter(|_| !self.queued.is_empty());
                        if self.queued.is_empty() && self.focus == FocusPane::Queue {
                            self.focus = FocusPane::Input;
                        }
                        // Build full history (includes the committed partial text).
                        let history = messages_for_resubmit(&self.chat_segments);
                        self.chat_segments
                            .push(ChatSegment::Message(Message::user(&next.content)));
                        self.save_history_async();
                        self.rerender_chat().await;
                        self.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_resubmit_to_agent(history, next).await;
                    }
                }
            }
            AgentEvent::Error(msg) => {
                self.chat_segments.push(ChatSegment::Error(msg.clone()));
                self.save_history_async();
                self.rerender_chat().await;
                self.agent_busy = false;
                self.current_tool = None;
            }
            AgentEvent::TodoUpdate(todos) => {
                let todo_md = format_todos_markdown(&todos);
                self.chat_segments
                    .push(ChatSegment::Message(Message::assistant(&todo_md)));
                self.save_history_async();
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
                self.save_history_async();
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
