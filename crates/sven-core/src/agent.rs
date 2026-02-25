// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use sven_config::{AgentConfig, AgentMode};
use sven_model::{
    CompletionRequest, FunctionCall, Message, MessageContent, ResponseEvent, Role,
};
use sven_tools::{events::ToolEvent, ToolCall, ToolOutput, ToolRegistry};

use crate::{
    compact::compact_session,
    events::AgentEvent,
    prompts::system_prompt,
    runtime_context::AgentRuntimeContext,
    session::Session,
};

/// The core agent.  Owns a session and drives the model ↔ tool loop.
pub struct Agent {
    session: Session,
    tools: Arc<ToolRegistry>,
    model: Arc<dyn sven_model::ModelProvider>,
    config: Arc<AgentConfig>,
    runtime: AgentRuntimeContext,
    /// Shared mode lock — the same Arc given to `SwitchModeTool` so that
    /// tool-driven mode changes are immediately visible to the agent loop.
    current_mode: Arc<Mutex<AgentMode>>,
    /// Receives `ToolEvent`s emitted by stateful tools (todo updates, mode
    /// changes).  The paired sender is held by `TodoWriteTool` /
    /// `SwitchModeTool` inside the registry.
    tool_event_rx: mpsc::Receiver<ToolEvent>,
}

impl Agent {
    /// Construct an agent.
    ///
    /// `mode_lock` must be the **same** `Arc` that was given to any
    /// `SwitchModeTool` in `tools`, so that mode changes propagate correctly.
    ///
    /// `tool_event_rx` must be the receiving end of the channel whose sender
    /// was given to `TodoWriteTool` / `SwitchModeTool`, so that tool events
    /// are drained by the agent loop.
    pub fn new(
        model: Arc<dyn sven_model::ModelProvider>,
        tools: Arc<ToolRegistry>,
        config: Arc<AgentConfig>,
        runtime: AgentRuntimeContext,
        mode_lock: Arc<Mutex<AgentMode>>,
        tool_event_rx: mpsc::Receiver<ToolEvent>,
        max_context_tokens: usize,
    ) -> Self {
        Self {
            session: Session::new(max_context_tokens),
            tools,
            model,
            config,
            runtime,
            current_mode: mode_lock,
            tool_event_rx,
        }
    }

    /// Replace the model provider for subsequent completions.
    ///
    /// Used by the CI runner to switch models mid-workflow (per-step model
    /// overrides).  The session history is preserved.
    pub fn set_model(&mut self, model: Arc<dyn sven_model::ModelProvider>) {
        // Update context window from the new model's catalog entry.
        if let Some(cw) = model.catalog_context_window() {
            self.session.max_tokens = cw as usize;
        }
        self.model = model;
    }

    /// Like [`submit`] but accepts a cancellation channel.
    ///
    /// When the sender half is dropped (or sends `()`) the current model
    /// streaming turn is interrupted at the next `await` point.  Any text
    /// already streamed is committed to the session as a partial assistant
    /// message and `AgentEvent::Aborted { partial_text }` is emitted so the
    /// TUI can handle it (e.g. keep it in the chat pane and suppress
    /// auto-dequeue).
    ///
    /// If `cancel` is already resolved on entry the submit is skipped
    /// entirely and `Aborted { partial_text: "" }` is emitted immediately.
    pub async fn submit_with_cancel(
        &mut self,
        user_input: &str,
        tx: mpsc::Sender<AgentEvent>,
        mut cancel: tokio::sync::oneshot::Receiver<()>,
    ) -> anyhow::Result<()> {
        // If already cancelled, emit Aborted immediately without touching history.
        if cancel.try_recv().is_ok() {
            let _ = tx.send(AgentEvent::Aborted { partial_text: String::new() }).await;
            return Ok(());
        }

        // All the same setup as `submit`, including compaction, system message
        // injection, and user message push — only the final loop call differs.
        let mode = *self.current_mode.lock().await;

        if self.session.is_near_limit(self.config.compaction_threshold) {
            let sys = self.system_message(mode);
            let tokens_before = self.session.token_count;
            let keep_n = self.config.compaction_keep_recent;

            let non_system: Vec<Message> = self.session.messages.iter()
                .filter(|m| m.role != Role::System)
                .cloned()
                .collect();

            let preserve_count = if non_system.len() > keep_n * 2 { keep_n } else { 0 };
            let summarize_count = non_system.len().saturating_sub(preserve_count);
            let recent_messages: Vec<Message> = non_system[summarize_count..].to_vec();
            let mut to_compact: Vec<Message> = non_system[..summarize_count].to_vec();
            compact_session(&mut to_compact, Some(sys.clone()));
            self.session.messages = to_compact;
            self.session.recalculate_tokens();

            let summary = self.run_single_turn(tx.clone(), mode).await?;
            self.session.messages.clear();
            self.session.messages.push(sys);
            self.session.messages.push(Message::assistant(summary));
            self.session.messages.extend(recent_messages);
            self.session.recalculate_tokens();

            let _ = tx.send(AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after: self.session.token_count,
            }).await;
        }

        if self.session.messages.is_empty() {
            self.session.push(self.system_message(mode));
        }
        self.session.push(Message::user(user_input));

        self.run_agentic_loop_cancellable(tx, &mut cancel).await
    }

    /// Like [`replace_history_and_submit`] but accepts a cancellation channel.
    pub async fn replace_history_and_submit_with_cancel(
        &mut self,
        messages: Vec<Message>,
        new_user_content: &str,
        tx: mpsc::Sender<AgentEvent>,
        mut cancel: tokio::sync::oneshot::Receiver<()>,
    ) -> anyhow::Result<()> {
        if cancel.try_recv().is_ok() {
            let _ = tx.send(AgentEvent::Aborted { partial_text: String::new() }).await;
            return Ok(());
        }

        let mode = *self.current_mode.lock().await;
        let mut msgs = messages;
        if msgs.is_empty() || msgs[0].role != Role::System {
            let sys = self.system_message(mode);
            msgs.insert(0, sys);
        }
        self.session.replace_messages(msgs);
        self.session.push(Message::user(new_user_content));

        self.run_agentic_loop_cancellable(tx, &mut cancel).await
    }

    /// Push a user message, run the agent loop, and stream events through the sender.
    /// The caller drops the receiver when it is no longer interested.
    pub async fn submit(
        &mut self,
        user_input: &str,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let mode = *self.current_mode.lock().await;

        // Proactive compaction before adding the new user message.
        //
        // Rolling strategy: preserve the `compaction_keep_recent` most recent
        // non-system messages verbatim so the model always has immediate
        // context, while older messages are condensed into a summary.
        if self.session.is_near_limit(self.config.compaction_threshold) {
            let sys = self.system_message(mode);
            let tokens_before = self.session.token_count;
            let keep_n = self.config.compaction_keep_recent;

            // Collect non-system messages and split into "to summarise" vs "to preserve".
            let non_system: Vec<Message> = self.session.messages.iter()
                .filter(|m| m.role != Role::System)
                .cloned()
                .collect();

            // Preserve the recent tail only when there are at least 2× keep_n
            // messages, so we always have a meaningful chunk to summarise.
            // When the history is shorter than that threshold we summarise
            // everything — the same behaviour as the original code — to avoid
            // producing a summary of just 1–2 messages that barely reduces
            // context usage and risks repeated compaction on every turn.
            let preserve_count = if non_system.len() > keep_n * 2 { keep_n } else { 0 };
            let summarize_count = non_system.len().saturating_sub(preserve_count);

            // Messages to keep verbatim after compaction (the recent tail).
            let recent_messages: Vec<Message> = non_system[summarize_count..].to_vec();

            // Build a temporary message list containing only the messages that
            // need to be summarised, then hand it to compact_session.
            let mut to_compact: Vec<Message> = non_system[..summarize_count].to_vec();
            compact_session(&mut to_compact, Some(sys.clone()));
            self.session.messages = to_compact;
            self.session.recalculate_tokens();

            // Ask the model to produce the summary.
            let summary = self.run_single_turn(tx.clone(), mode).await?;

            // Rebuild session: system → summary → preserved recent messages.
            self.session.messages.clear();
            self.session.messages.push(sys);
            self.session.messages.push(Message::assistant(summary));
            self.session.messages.extend(recent_messages);
            self.session.recalculate_tokens();

            let _ = tx.send(AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after: self.session.token_count,
            }).await;
        }

        // Inject system message if this is the first turn
        if self.session.messages.is_empty() {
            self.session.push(self.system_message(mode));
        }

        self.session.push(Message::user(user_input));
        self.run_agentic_loop(tx).await
    }

    /// Push a multimodal user message (text + images), then run the agent loop.
    ///
    /// Use this when the caller wants to attach one or more images to the user
    /// turn.  Images that the current model does not support will be stripped
    /// transparently before the first model call.
    pub async fn submit_with_parts(
        &mut self,
        parts: Vec<sven_model::ContentPart>,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let mode = *self.current_mode.lock().await;
        if self.session.messages.is_empty() {
            self.session.push(self.system_message(mode));
        }
        self.session.push(Message::user_with_parts(parts));
        self.run_agentic_loop(tx).await
    }

    /// Pre-load conversation history into the session without submitting.
    ///
    /// Used when piped input is detected to be conversation-format markdown:
    /// the prior turns become context so the next `submit()` call continues
    /// the conversation rather than starting fresh.
    ///
    /// System messages in `messages` are stripped — the correct system message
    /// is injected automatically by `submit()` / `replace_history_and_submit`.
    pub async fn seed_history(&mut self, messages: Vec<Message>) {
        let mode = *self.current_mode.lock().await;
        let mut msgs: Vec<Message> = messages
            .into_iter()
            .filter(|m| m.role != Role::System)
            .collect();
        if !msgs.is_empty() {
            let sys = self.system_message(mode);
            msgs.insert(0, sys);
            self.session.replace_messages(msgs);
        }
    }

    /// Replace session history with the given messages, then run with the new user message.
    /// Used for edit-and-resubmit: TUI sends truncated history + new user content.
    /// Prepends system message if the list does not start with one.
    pub async fn replace_history_and_submit(
        &mut self,
        messages: Vec<Message>,
        new_user_content: &str,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let mode = *self.current_mode.lock().await;
        let mut msgs = messages;
        if msgs.is_empty() || msgs[0].role != Role::System {
            let sys = self.system_message(mode);
            msgs.insert(0, sys);
        }
        self.session.replace_messages(msgs);
        self.session.push(Message::user(new_user_content));
        self.run_agentic_loop(tx).await
    }

    /// Cancellable version of [`run_agentic_loop`].
    ///
    /// Checks `cancel` at the top of every iteration and inside
    /// `stream_one_turn` via `select!`.  When cancelled, any text already
    /// streamed is committed to the session and `AgentEvent::Aborted` is sent.
    async fn run_agentic_loop_cancellable(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut rounds = 0u32;
        let mut partial_text = String::new();

        loop {
            // Check cancel before each round.
            if cancel.try_recv().is_ok() {
                if !partial_text.is_empty() {
                    self.session.push(Message::assistant(&partial_text));
                }
                let _ = tx.send(AgentEvent::Aborted { partial_text }).await;
                return Ok(());
            }

            rounds += 1;
            if rounds > self.config.max_tool_rounds {
                // Instead of hard-stopping with an error, give the model one
                // final tool-free turn so it can summarise what it completed.
                // This avoids leaving the task in a half-finished state and
                // keeps the turn ending as `TurnComplete` (not `Error`), so
                // the TUI does not show a red error and the user can continue.
                let wrap_msg = format!(
                    "You have reached the maximum tool-call budget ({} rounds). \
                     Do not call any more tools. \
                     Write a concise summary of: (1) what has been completed, \
                     (2) what still remains to be done, and (3) how to continue.",
                    self.config.max_tool_rounds
                );
                self.session.push(Message::user(&wrap_msg));

                let mode = *self.current_mode.lock().await;
                let wrap_turn = tokio::select! {
                    biased;
                    _ = &mut *cancel => None,
                    result = self.stream_one_turn(tx.clone(), mode, false) => Some(result),
                };
                if let Some(Ok((text, _, _))) = wrap_turn {
                    if !text.is_empty() {
                        self.session.push(Message::assistant(&text));
                    }
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            let mode = *self.current_mode.lock().await;

            // stream_one_turn_cancellable returns None when the cancel channel fires.
            let turn = tokio::select! {
                biased;
                _ = &mut *cancel => None,
                result = self.stream_one_turn(tx.clone(), mode, true) => Some(result),
            };

            let (text, tool_calls, had_tool_calls) = match turn {
                None => {
                    // Aborted mid-stream.  The streaming buffer has the partial text.
                    if !partial_text.is_empty() {
                        self.session.push(Message::assistant(&partial_text));
                    }
                    let _ = tx.send(AgentEvent::Aborted { partial_text }).await;
                    return Ok(());
                }
                Some(Err(e)) => return Err(e),
                Some(Ok(t)) => t,
            };

            // Accumulate text for abort recovery.
            if !text.is_empty() {
                partial_text.push_str(&text);
                self.session.push(Message::assistant(&text));
            }

            if !had_tool_calls {
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            // Phase 1: push all assistant tool-call messages.
            for tc in &tool_calls {
                let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                self.session.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                });
            }

            // Phase 2: execute tools in parallel.
            let mut tasks = Vec::with_capacity(tool_calls.len());
            for tc in tool_calls.clone() {
                let registry = Arc::clone(&self.tools);
                tasks.push(tokio::spawn(async move { registry.execute(&tc).await }));
            }

            let mut outputs = Vec::with_capacity(tool_calls.len());
            for (i, task) in tasks.into_iter().enumerate() {
                let output = match task.await {
                    Ok(o) => o,
                    Err(e) => ToolOutput::err(&tool_calls[i].id, format!("tool panicked: {e}")),
                };
                self.drain_tool_events(&tx).await;
                let _ = tx.send(AgentEvent::ToolCallFinished {
                    call_id: tool_calls[i].id.clone(),
                    tool_name: tool_calls[i].name.clone(),
                    output: output.content.clone(),
                    is_error: output.is_error,
                }).await;
                outputs.push(output);
            }

            // Phase 3: push tool-result messages.
            for (tc, output) in tool_calls.iter().zip(outputs.iter()) {
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output.parts.iter().map(|p| match p {
                        sven_tools::ToolOutputPart::Text(t) =>
                            ToolContentPart::Text { text: t.clone() },
                        sven_tools::ToolOutputPart::Image(url) =>
                            ToolContentPart::Image { image_url: url.clone() },
                    }).collect();
                    Message::tool_result_with_parts(&tc.id, parts)
                } else {
                    Message::tool_result(&tc.id, &output.content)
                };
                self.session.push(tool_msg);
            }
        }

        Ok(())
    }

    /// The main agent loop: model call → optional tool calls → repeat
    async fn run_agentic_loop(&mut self, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let mut rounds = 0u32;

        loop {
            rounds += 1;
            if rounds > self.config.max_tool_rounds {
                // Give the model one final tool-free turn to summarise its
                // progress rather than stopping abruptly with an error.
                let wrap_msg = format!(
                    "You have reached the maximum tool-call budget ({} rounds). \
                     Do not call any more tools. \
                     Write a concise summary of: (1) what has been completed, \
                     (2) what still remains to be done, and (3) how to continue.",
                    self.config.max_tool_rounds
                );
                self.session.push(Message::user(&wrap_msg));

                let mode = *self.current_mode.lock().await;
                let (text, _, _) = self.stream_one_turn(tx.clone(), mode, false).await?;
                if !text.is_empty() {
                    self.session.push(Message::assistant(&text));
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            let mode = *self.current_mode.lock().await;
            let (text, tool_calls, had_tool_calls) =
                self.stream_one_turn(tx.clone(), mode, true).await?;

            if !text.is_empty() {
                self.session.push(Message::assistant(&text));
            }

            if !had_tool_calls {
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            // Phase 1: push all assistant tool-call messages (must all come
            // before any tool-result messages for OpenAI's parallel-tool-call
            // wire format).
            for tc in &tool_calls {
                let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                self.session.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                });
            }

            // Phase 2: execute all tools in parallel using tokio::spawn.
            // Each task gets a cloned Arc to the registry (cheap, atomic refcount).
            // Tasks are isolated — one panic doesn't cancel others.
            let mut tasks = Vec::with_capacity(tool_calls.len());
            for tc in tool_calls.clone() {
                let registry = Arc::clone(&self.tools);
                let task = tokio::spawn(async move {
                    registry.execute(&tc).await
                });
                tasks.push(task);
            }

            // Await all tasks in order, preserving result indices for correct
            // conversation history serialization.
            let mut outputs = Vec::with_capacity(tool_calls.len());
            for (i, task) in tasks.into_iter().enumerate() {
                let output = match task.await {
                    Ok(output) => output,
                    Err(e) => {
                        // Task panicked — treat as tool error
                        ToolOutput::err(
                            &tool_calls[i].id,
                            format!("tool execution panicked: {}", e),
                        )
                    }
                };

                // Drain tool events (may arrive from any task via shared channel)
                self.drain_tool_events(&tx).await;

                let _ = tx.send(AgentEvent::ToolCallFinished {
                    call_id: tool_calls[i].id.clone(),
                    tool_name: tool_calls[i].name.clone(),
                    output: output.content.clone(),
                    is_error: output.is_error,
                }).await;

                outputs.push(output);
            }

            // Phase 3: push all tool-result messages.
            for (tc, output) in tool_calls.iter().zip(outputs.iter()) {
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output.parts.iter().map(|p| match p {
                        sven_tools::ToolOutputPart::Text(t) => ToolContentPart::Text { text: t.clone() },
                        sven_tools::ToolOutputPart::Image(url) => ToolContentPart::Image { image_url: url.clone() },
                    }).collect();
                    Message::tool_result_with_parts(&tc.id, parts)
                } else {
                    Message::tool_result(&tc.id, &output.content)
                };
                self.session.push(tool_msg);
            }
        }

        Ok(())
    }

    /// Drain pending tool events and translate to AgentEvents.
    async fn drain_tool_events(&mut self, tx: &mpsc::Sender<AgentEvent>) {
        while let Ok(te) = self.tool_event_rx.try_recv() {
            match te {
                ToolEvent::TodoUpdate(todos) => {
                    let _ = tx.send(AgentEvent::TodoUpdate(todos)).await;
                }
                ToolEvent::ModeChanged(new_mode) => {
                    *self.current_mode.lock().await = new_mode;
                    let _ = tx.send(AgentEvent::ModeChanged(new_mode)).await;
                }
            }
        }
    }

    /// Call the model once, streaming text deltas and collecting tool-call events.
    /// Returns (full_text, tool_calls, had_tool_calls).
    async fn stream_one_turn(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        mode: AgentMode,
        with_tools: bool,
    ) -> anyhow::Result<(String, Vec<ToolCall>, bool)> {
        let tools: Vec<sven_model::ToolSchema> = if with_tools {
            self.tools.schemas_for_mode(mode)
                .into_iter()
                .map(|s| sven_model::ToolSchema {
                    name: s.name,
                    description: s.description,
                    parameters: s.parameters,
                })
                .collect()
        } else {
            vec![]
        };

        // Strip image content when the current model does not support images.
        let modalities = self.model.input_modalities();
        let messages = sven_model::sanitize::strip_images_if_unsupported(
            self.session.messages.clone(),
            &modalities,
        );

        let req = CompletionRequest {
            messages: messages.clone(),
            tools: tools.clone(),
            stream: true,
            // Carry volatile context (git/CI) separately so providers that
            // support prompt caching (Anthropic) can put it in an uncached
            // system block while the stable prefix stays cached.
            system_dynamic_suffix: self.dynamic_context(),
            // Stable session identifier forwarded to providers that support
            // an explicit cache key (e.g. OpenRouter's prompt_cache_key).
            cache_key: Some(self.session.id.clone()),
        };

        let mut stream = self.model.complete(req).await
            .context("model completion failed")?;

        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        // Keyed by the parallel-tool-call index from the provider.
        // OpenAI interleaves chunks for different tool calls by index;
        // other providers always use index 0.
        let mut pending_tcs: HashMap<u32, PendingToolCall> = HashMap::new();
        // Accumulate thinking deltas so we can emit a single ThinkingComplete
        // event to consumers (CI runner, TUI) once the thinking block ends.
        let mut thinking_buf = String::new();

        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::ThinkingDelta(delta) => {
                    thinking_buf.push_str(&delta);
                    let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
                }
                ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                    // Flush accumulated thinking when text starts arriving.
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
                    }
                    full_text.push_str(&delta);
                    let _ = tx.send(AgentEvent::TextDelta(delta)).await;
                }
                ResponseEvent::ToolCall { index, id, name, arguments } => {
                    let ptc = pending_tcs.entry(index).or_insert_with(|| PendingToolCall {
                        id: String::new(),
                        name: String::new(),
                        args_buf: String::new(),
                    });
                    if !id.is_empty() { ptc.id = id; }
                    if !name.is_empty() { ptc.name = name; }
                    ptc.args_buf.push_str(&arguments);
                }
                ResponseEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens } => {
                    self.session.add_cache_usage(cache_read_tokens, cache_write_tokens);
                    let _ = tx.send(AgentEvent::TokenUsage {
                        input: input_tokens,
                        output: output_tokens,
                        cache_read: cache_read_tokens,
                        cache_write: cache_write_tokens,
                        cache_read_total: self.session.cache_read_total,
                        cache_write_total: self.session.cache_write_total,
                        max_tokens: self.session.max_tokens,
                    }).await;
                }
                ResponseEvent::Done => {
                    // Flush any trailing thinking block (model thought without responding).
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
                    }
                    break;
                }
                ResponseEvent::Error(e) => {
                    warn!("model stream error: {e}");
                }
                _ => {}
            }
        }

        // Flush all accumulated parallel tool calls, ordered by index.
        // Tool calls with an empty name cannot be dispatched and are dropped —
        // storing them would corrupt the conversation history sent back to the
        // API on the next turn.  An empty id (which violates Anthropic's
        // `^[a-zA-Z0-9_-]+$` constraint) gets a synthetic fallback so the
        // turn can still be completed without a spurious 400 error.
        let mut pending_sorted: Vec<(u32, PendingToolCall)> = pending_tcs.into_iter().collect();
        pending_sorted.sort_by_key(|(idx, _)| *idx);
        for (i, (_, ptc)) in pending_sorted.into_iter().enumerate() {
            if ptc.name.is_empty() {
                warn!(
                    tool_call_id = %ptc.id,
                    "dropping tool call with empty name from model; cannot dispatch"
                );
                continue;
            }
            let mut tc = ptc.finish();
            if tc.id.is_empty() {
                tc.id = format!("tc_synthetic_{i}");
                warn!(
                    tool_name = %tc.name,
                    tool_call_id = %tc.id,
                    "tool call from model had empty id; generated synthetic id"
                );
            }
            tool_calls.push(tc);
        }

        if !full_text.is_empty() {
            let _ = tx.send(AgentEvent::TextComplete(full_text.clone())).await;
        }

        let had_tool_calls = !tool_calls.is_empty();
        Ok((full_text, tool_calls, had_tool_calls))
    }

    /// Run a single turn (no tool loop) and return the full text response.
    async fn run_single_turn(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        mode: AgentMode,
    ) -> anyhow::Result<String> {
        let (text, _, _) = self.stream_one_turn(tx, mode, true).await?;
        Ok(text)
    }

    fn system_message(&self, mode: AgentMode) -> Message {
        let ctx = self.prompt_context();
        // Use the STABLE portion only — volatile context (git/CI) is injected
        // per-request via `system_dynamic_suffix` so it does not break prompt
        // caching across sessions.
        let stable_ctx = ctx.stable_only();
        let custom = self.runtime.system_prompt_override.as_deref()
            .or(self.config.system_prompt.as_deref());
        Message::system(system_prompt(
            mode,
            custom,
            &self.tools.names_for_mode(mode),
            stable_ctx,
        ))
    }

    /// Build a `PromptContext` from the current runtime environment.
    fn prompt_context(&self) -> crate::prompts::PromptContext<'_> {
        crate::prompts::PromptContext {
            project_root: self.runtime.project_root.as_deref(),
            git_context: self.runtime.git_context_note.as_deref(),
            project_context_file: self.runtime.project_context_file.as_deref(),
            ci_context: self.runtime.ci_context_note.as_deref(),
            append: self.runtime.append_system_prompt.as_deref(),
        }
    }

    /// Volatile context (git + CI) formatted for injection as an uncached
    /// system block.  Returns `None` when no dynamic context is configured.
    fn dynamic_context(&self) -> Option<String> {
        // When a custom system prompt override is in use, the caller controls
        // all content — skip the dynamic injection to avoid duplication.
        if self.runtime.system_prompt_override.is_some()
            || self.config.system_prompt.is_some()
        {
            return None;
        }
        self.prompt_context().dynamic_block()
    }

    pub fn session(&self) -> &Session { &self.session }

    pub fn session_mut(&mut self) -> &mut Session { &mut self.session }

    pub fn mode(&self) -> AgentMode {
        *self.current_mode.blocking_lock()
    }

    /// Override the agent's current mode.  Takes effect on the next
    /// `submit` call (the new mode is used to build the system message and
    /// select the available tool set).
    pub async fn set_mode(&self, mode: AgentMode) {
        let mut m = self.current_mode.lock().await;
        *m = mode;
    }
}

struct PendingToolCall {
    id: String,
    name: String,
    args_buf: String,
}

impl PendingToolCall {
    fn finish(self) -> ToolCall {
        // Always resolve to a JSON object.  Model providers (notably Anthropic)
        // require tool_use input to be an object; sending `null` causes a 400
        // on the *next* completion request and surfaces as "model completion failed".
        let args = if self.args_buf.is_empty() {
            warn!(
                tool_name = %self.name,
                tool_call_id = %self.id,
                "model sent tool call with empty arguments; substituting {{}}"
            );
            serde_json::Value::Object(Default::default())
        } else {
            match serde_json::from_str(&self.args_buf) {
                Ok(v) => v,
                Err(e) => {
                    // Attempt to repair common JSON errors before giving up
                    match attempt_json_repair(&self.args_buf) {
                        Ok(v) => {
                            warn!(
                                tool_name = %self.name,
                                tool_call_id = %self.id,
                                "repaired invalid JSON arguments from model"
                            );
                            v
                        }
                        Err(_) => {
                            warn!(
                                tool_name = %self.name,
                                tool_call_id = %self.id,
                                args_buf = %self.args_buf,
                                error = %e,
                                "model sent tool call with invalid JSON arguments; substituting {{}}"
                            );
                            serde_json::Value::Object(Default::default())
                        }
                    }
                }
            }
        };
        ToolCall { id: self.id, name: self.name, args }
    }
}

/// Attempt to repair common JSON syntax errors.
/// 
/// This handles issues like:
/// - Missing commas between key-value pairs
/// - Missing quotes around keys or values
/// - Truncated strings
fn attempt_json_repair(json_str: &str) -> anyhow::Result<serde_json::Value> {
    // Try simple repairs in sequence
    
    // 1. Fix missing comma between key-value pairs like: "key1"value": "...
    // Pattern: "key"VALUE": where VALUE is alphanumeric
    let repaired = regex::Regex::new(r#""([^"]+)"([a-zA-Z_][a-zA-Z0-9_]*)":\s*"#)
        .unwrap()
        .replace_all(json_str, r#""$1", "$2": "#);
    
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&repaired) {
        return Ok(v);
    }
    
    // 2. Try adding missing closing quote and brace if JSON ends abruptly
    if !json_str.trim().ends_with('}') {
        let mut completed = json_str.to_string();
        // Count quotes to see if we need a closing quote
        let quote_count = json_str.chars().filter(|&c| c == '"').count();
        if quote_count % 2 == 1 {
            completed.push('"');
        }
        if !completed.trim().ends_with('}') {
            completed.push('}');
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&completed) {
            return Ok(v);
        }
    }
    
    // All repair attempts failed
    anyhow::bail!("JSON repair failed: all repair strategies exhausted")
}
