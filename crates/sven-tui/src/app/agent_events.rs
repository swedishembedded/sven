// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent event and question-request handlers.

use std::time::Instant;

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
                self.chat.streaming_is_thinking = false;
                // Approximate token count: ~4 chars per token.
                self.agent.streaming_tokens = self
                    .agent
                    .streaming_tokens
                    .saturating_add((delta.len() as u32 + 3) / 4);
                // Advance spinner frame.
                self.agent.spinner_frame = self.agent.spinner_frame.wrapping_add(1);
                self.chat.streaming_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
            }
            AgentEvent::TextComplete(full_text) => {
                self.chat
                    .segments
                    .push(ChatSegment::Message(Message::assistant(&full_text)));
                self.chat.streaming_buffer.clear();
                self.chat.streaming_is_thinking = false;
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
            }
            AgentEvent::ToolCallStarted(tc) => {
                self.chat.tool_args.insert(tc.id.clone(), tc.name.clone());
                self.agent.current_tool = Some(tc.name.clone());
                // Record start time for elapsed-time display.
                self.agent
                    .tool_start_times
                    .insert(tc.id.clone(), Instant::now());
                let seg_idx = self.chat.segments.len();
                self.chat.segments.push(ChatSegment::Message(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                }));
                if self.nvim.disabled {
                    // Default expand level for tool calls is 0 (summary).
                    // The HashMap default is used so no explicit insert needed.
                    let _ = seg_idx;
                }
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
            }
            AgentEvent::ToolCallFinished {
                call_id,
                output,
                is_error,
                ..
            } => {
                self.agent.current_tool = None;
                // Compute elapsed time from the recorded start.
                if let Some(start) = self.agent.tool_start_times.remove(&call_id) {
                    let elapsed = start.elapsed().as_secs_f32();
                    self.chat.tool_durations.insert(call_id.clone(), elapsed);
                }
                let seg_idx = self.chat.segments.len();
                let output_with_error = if is_error {
                    format!("error: {output}")
                } else {
                    output
                };
                self.chat
                    .segments
                    .push(ChatSegment::Message(Message::tool_result(
                        &call_id,
                        &output_with_error,
                    )));
                let _ = seg_idx;
                // Signal the run-loop to check and restore terminal state.
                self.needs_terminal_recover = true;
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
            }
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            } => {
                self.chat.segments.push(ChatSegment::ContextCompacted {
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
                output,
                cache_read,
                cache_write,
                max_tokens,
                max_output_tokens,
                ..
            } => {
                // Input side: update when provider reports prompt token counts.
                // For Anthropic this arrives in the message_start event at the
                // beginning of each API call within the turn.
                if input > 0 || cache_read > 0 || cache_write > 0 {
                    let total_ctx = input + cache_read + cache_write;
                    // Use the usable input budget as the denominator so that
                    // ctx% matches the fraction used for compaction decisions.
                    // Falls back to max_tokens when max_output_tokens is 0.
                    let input_budget = if max_output_tokens > 0 {
                        max_tokens.saturating_sub(max_output_tokens)
                    } else if max_tokens > 0 {
                        max_tokens
                    } else {
                        200_000
                    } as u32;
                    self.agent.context_pct = (total_ctx * 100 / input_budget).min(100) as u8;
                    self.agent.context_tokens = total_ctx;
                    // Mirror into total_context_tokens immediately so the status bar
                    // shows the current context size during streaming (not just after
                    // TurnComplete).  total_context_tokens tracks the latest context
                    // window size, NOT a running sum across turns.
                    self.agent.total_context_tokens = total_ctx;
                    self.agent.total_context_pct = self.agent.context_pct;
                    self.agent.cache_hit_pct = if total_ctx > 0 && cache_read > 0 {
                        (cache_read * 100 / total_ctx).min(100) as u8
                    } else {
                        0
                    };
                    self.agent.max_tokens = max_tokens;
                    self.agent.max_output_tokens = max_output_tokens;
                }
                // Output side: accumulate across all API calls within the turn.
                // For Anthropic, output tokens arrive in the message_delta usage
                // event at the end of each API call.  A single user turn may
                // involve several API calls (tool-use loop), so we must add here
                // rather than overwrite to capture every call's output tokens.
                if output > 0 {
                    self.agent.output_tokens += output;
                    // Discard the streaming estimate once the exact count is in.
                    self.agent.streaming_tokens = 0;
                }
            }
            AgentEvent::TurnComplete => {
                self.agent.busy = false;
                self.agent.current_tool = None;
                self.chat.tool_streaming_content.clear();
                // Preserve the final context size from this turn before reset.
                // total_context_tokens tracks the current context window size
                // (NOT a running sum), so use = not +=.
                self.agent.total_context_tokens = self.agent.context_tokens;
                // Accumulate output tokens: context_tokens is per-turn (reset each
                // turn), but output_tokens is a true cumulative billing metric.
                self.agent.total_output_tokens += self.agent.output_tokens;
                // total_context_pct was already kept in sync by the TokenUsage
                // handler; no recalculation needed here.
                // Reset per-turn counts for the next turn.
                self.agent.context_tokens = 0;
                self.agent.output_tokens = 0;
                self.agent.streaming_tokens = 0;
                self.agent.spinner_frame = 0;
                self.agent.tool_start_times.clear();
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.set_modifiable(true).await {
                        tracing::error!("Failed to set buffer modifiable: {}", e);
                    }
                }
                self.save_history_async();
                // Only dequeue the next message if no queue item is being edited
                // and an abort did not explicitly suppress auto-advance.
                if self.edit.queue_index.is_none() && !self.queue.abort_pending {
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
                        self.save_history_async();
                        self.rerender_chat().await;
                        self.chat.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_to_agent(next).await;
                    }
                }
            }
            AgentEvent::Aborted { partial_text } => {
                self.chat.streaming_buffer.clear();
                self.chat.streaming_is_thinking = false;
                if !partial_text.is_empty() {
                    self.chat
                        .segments
                        .push(ChatSegment::Message(Message::assistant(&partial_text)));
                }
                self.agent.busy = false;
                self.agent.current_tool = None;
                // Preserve the final context size from this partial turn.
                self.agent.total_context_tokens = self.agent.context_tokens;
                self.agent.total_output_tokens += self.agent.output_tokens;
                // Reset per-turn counts.
                self.agent.context_tokens = 0;
                self.agent.output_tokens = 0;
                self.agent.streaming_tokens = 0;
                self.agent.spinner_frame = 0;
                self.agent.tool_start_times.clear();
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    let _ = bridge.set_modifiable(true).await;
                }
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                if let Some(pager) = &mut self.ui.pager {
                    pager.set_lines(self.chat.lines.clone());
                }
                // If abort_pending is set, the user did a plain /abort — keep
                // the queue as-is and wait for manual submit.
                // If abort_pending is false (force-submit path), auto-dequeue.
                if !self.queue.abort_pending && self.edit.queue_index.is_none() {
                    if let Some(next) = self.queue.messages.pop_front() {
                        self.queue.selected = self
                            .queue
                            .selected
                            .map(|s| s.saturating_sub(1))
                            .filter(|_| !self.queue.messages.is_empty());
                        if self.queue.messages.is_empty() && self.ui.focus == FocusPane::Queue {
                            self.ui.focus = FocusPane::Input;
                        }
                        let history = messages_for_resubmit(&self.chat.segments);
                        self.chat
                            .segments
                            .push(ChatSegment::Message(Message::user(&next.content)));
                        self.save_history_async();
                        self.rerender_chat().await;
                        self.chat.auto_scroll = true;
                        self.scroll_to_bottom();
                        self.send_resubmit_to_agent(history, next).await;
                    }
                }
            }
            AgentEvent::ToolProgress { call_id, message } => {
                // Messages prefixed "[stream_buf:<handle>]\n" carry live sub-agent
                // output.  Store the content for the expanded view; update the
                // spinner label with a short status summary.
                const STREAM_PREFIX: &str = "[stream_buf:";
                if message.starts_with(STREAM_PREFIX) {
                    if let Some(rest) = message.strip_prefix(STREAM_PREFIX) {
                        // Format: "[stream_buf:<handle>]\nlines:<n>\n<tail>"
                        let bracket = rest.find(']').unwrap_or(rest.len());
                        let handle = &rest[..bracket];
                        let after_bracket = rest.get(bracket + 2..).unwrap_or(""); // skip "]\n"
                        self.chat
                            .tool_streaming_content
                            .insert(call_id.clone(), after_bracket.to_string());
                        // Status line (first line) for spinner label.
                        let status_line = after_bracket.lines().next().unwrap_or("running");
                        self.agent.current_tool =
                            Some(format!("task [{}] {}", handle, status_line));
                    }
                } else {
                    // Regular progress message — just update the spinner label.
                    self.agent.current_tool = Some(message);
                }
                self.rerender_chat().await;
            }
            AgentEvent::Error(msg) => {
                self.chat.segments.push(ChatSegment::Error(msg.clone()));
                self.save_history_async();
                self.rerender_chat().await;
                self.agent.busy = false;
                self.agent.current_tool = None;
            }
            AgentEvent::TodoUpdate(todos) => {
                let todo_md = format_todos_markdown(&todos);
                self.chat
                    .segments
                    .push(ChatSegment::Message(Message::assistant(&todo_md)));
                self.save_history_async();
                self.rerender_chat().await;
                if let Some(nvim_bridge) = &self.nvim.bridge {
                    let mut bridge = nvim_bridge.lock().await;
                    if let Err(e) = bridge.refresh_todo_display().await {
                        tracing::warn!("Failed to refresh todo display: {}", e);
                    }
                }
            }
            AgentEvent::ThinkingDelta(delta) => {
                self.chat.streaming_is_thinking = true;
                self.agent.spinner_frame = self.agent.spinner_frame.wrapping_add(1);
                self.chat.streaming_buffer.push_str(&delta);
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
            AgentEvent::ThinkingComplete(content) => {
                self.chat.streaming_buffer.clear();
                self.chat.streaming_is_thinking = false;
                self.chat.segments.push(ChatSegment::Thinking { content });
                // Default expand level is 0 (summary) — no explicit insert needed.
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
                self.nvim_scroll_to_bottom().await;
            }
            AgentEvent::ModeChanged(mode) => {
                self.session.mode = mode;
            }
            AgentEvent::CollabEvent(ev) => {
                self.chat.segments.push(ChatSegment::CollabEvent(ev));
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
            AgentEvent::DelegateSummary {
                to_name,
                task_title,
                duration_ms,
                status,
                result_preview,
            } => {
                self.chat.segments.push(ChatSegment::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                    expanded: false,
                    inner: vec![],
                });
                self.save_history_async();
                self.rerender_chat().await;
                self.scroll_to_bottom();
            }
            _ => {}
        }
        false
    }

    // ── Question request handler ──────────────────────────────────────────────

    pub(crate) fn handle_question_request(&mut self, req: QuestionRequest) {
        tracing::debug!(id = %req.id, count = req.questions.len(), "question request received");
        self.ui.question_modal = Some(QuestionModal::new(req.questions, req.answer_tx));
        self.ui.focus = FocusPane::Input;
    }
}
