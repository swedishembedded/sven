// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use sven_config::{AgentConfig, AgentMode, CompactionStrategy};
use sven_model::{
    CompletionRequest, FunctionCall, Message, MessageContent, ResponseEvent, Role,
};
use sven_tools::{events::ToolEvent, ToolCall, ToolOutput, ToolRegistry};

use crate::{
    compact::{compact_session_with_strategy, emergency_compact, smart_truncate},
    events::{AgentEvent, CompactionStrategyUsed},
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
        let max_output_tokens = model.catalog_max_output_tokens().unwrap_or(0) as usize;
        let mut session = Session::new(max_context_tokens);
        session.max_output_tokens = max_output_tokens;
        Self {
            session,
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
    /// Returns a shared reference to the tool registry.
    /// Used by the CI runner to execute tool calls outside the normal agent loop
    /// (e.g. `--rerun-toolcalls`).
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    /// Used by the CI runner to switch models mid-workflow (per-step model
    /// overrides).  The session history is preserved.
    pub fn set_model(&mut self, model: Arc<dyn sven_model::ModelProvider>) {
        // Update context window and output token limit from the new model's catalog.
        if let Some(cw) = model.catalog_context_window() {
            self.session.max_tokens = cw as usize;
        }
        if let Some(mot) = model.catalog_max_output_tokens() {
            self.session.max_output_tokens = mot as usize;
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

        self.ensure_fits_budget(&tx, mode, 0).await?;

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
        self.ensure_fits_budget(&tx, mode, 0).await?;

        // Inject system message if this is the first turn.
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

        // Proactive compaction before adding the new user message.
        self.ensure_fits_budget(&tx, mode, 0).await?;

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

        // Proactive compaction after loading the (potentially large) history.
        self.ensure_fits_budget(&tx, mode, 0).await?;

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
            // We treat both an explicit send(()) AND a dropped sender as a
            // cancellation signal.  `send_abort_signal` drops the sender half
            // without sending, so `try_recv()` returns `Err(Closed)` in that
            // case — which would be missed by a plain `.is_ok()` check.
            match cancel.try_recv() {
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                _ => {
                    if !partial_text.is_empty() {
                        self.session.push(Message::assistant(&partial_text));
                    }
                    let _ = tx.send(AgentEvent::Aborted { partial_text }).await;
                    return Ok(());
                }
            }

            rounds += 1;
            if rounds > self.config.max_tool_rounds {
                // Instead of hard-stopping with an error, give the model one
                // final tool-free turn so it can summarise what it completed.
                let wrap_msg = format!(
                    "You have reached the maximum tool-call budget ({} rounds). \
                     Do not call any more tools. \
                     Write a concise summary of: (1) what has been completed, \
                     (2) what still remains to be done, and (3) how to continue.",
                    self.config.max_tool_rounds
                );
                self.session.push(Message::user(&wrap_msg));

                let mode = *self.current_mode.lock().await;
                self.session.schema_overhead = self.estimate_schema_overhead(mode);
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
            // Update schema overhead for accurate budget calculations.
            self.session.schema_overhead = self.estimate_schema_overhead(mode);

            let turn = tokio::select! {
                biased;
                _ = &mut *cancel => None,
                result = self.stream_one_turn(tx.clone(), mode, true) => Some(result),
            };

            let (text, tool_calls, had_tool_calls) = match turn {
                None => {
                    // Aborted mid-stream.
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

            // Phase 3: push tool-result messages with smart truncation.
            let cap = self.config.tool_result_token_cap;
            for (tc, output) in tool_calls.iter().zip(outputs.iter()) {
                let category = self.tools.output_category(&tc.name);
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output.parts.iter().map(|p| match p {
                        sven_tools::ToolOutputPart::Text(t) => {
                            let truncated = smart_truncate(t, category, cap);
                            ToolContentPart::Text { text: truncated }
                        }
                        sven_tools::ToolOutputPart::Image(url) => {
                            ToolContentPart::Image { image_url: url.clone() }
                        }
                    }).collect();
                    Message::tool_result_with_parts(&tc.id, parts)
                } else {
                    let content = smart_truncate(&output.content, category, cap);
                    Message::tool_result(&tc.id, &content)
                };
                self.session.push(tool_msg);
            }

            // Mid-loop budget gate.
            self.ensure_fits_budget(&tx, mode, rounds).await?;
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
                self.session.schema_overhead = self.estimate_schema_overhead(mode);
                let (text, _, _) = self.stream_one_turn(tx.clone(), mode, false).await?;
                if !text.is_empty() {
                    self.session.push(Message::assistant(&text));
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            let mode = *self.current_mode.lock().await;
            // Update schema overhead so the budget gate and calibration are
            // accurate for this turn's actual request size.
            self.session.schema_overhead = self.estimate_schema_overhead(mode);
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

            // Phase 3: push all tool-result messages, applying smart truncation
            // when a result exceeds the configured token cap.
            let cap = self.config.tool_result_token_cap;
            for (tc, output) in tool_calls.iter().zip(outputs.iter()) {
                let category = self.tools.output_category(&tc.name);
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output.parts.iter().map(|p| match p {
                        sven_tools::ToolOutputPart::Text(t) => {
                            let truncated = smart_truncate(t, category, cap);
                            ToolContentPart::Text { text: truncated }
                        }
                        sven_tools::ToolOutputPart::Image(url) => {
                            ToolContentPart::Image { image_url: url.clone() }
                        }
                    }).collect();
                    Message::tool_result_with_parts(&tc.id, parts)
                } else {
                    let content = smart_truncate(&output.content, category, cap);
                    Message::tool_result(&tc.id, &content)
                };
                self.session.push(tool_msg);
            }

            // Mid-loop budget gate: after tool results are pushed, check
            // whether the session now exceeds the compaction threshold.
            // This prevents a single large tool output from causing a hard
            // failure on the next model call.
            self.ensure_fits_budget(&tx, mode, rounds).await?;
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
        // Set to true when the provider signals that the output was cut off by
        // the max-tokens limit.  Used to recover partial tool-call arguments.
        let mut output_was_truncated = false;

        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::MaxTokens => {
                    output_was_truncated = true;
                }
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
                    // Update the running calibration factor using the provider's
                    // actual input token count.  This corrects the chars/4
                    // approximation for the current workload and model.
                    let actual_input = input_tokens + cache_read_tokens;
                    if actual_input > 0 {
                        let estimated = self.session.token_count + self.session.schema_overhead;
                        self.session.update_calibration(actual_input, estimated);
                    }
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
            let mut tc = ptc.finish(output_was_truncated);
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

    /// Run a single tool-free turn and return the full text response.
    /// Used for compaction summary generation; no tools are passed so the
    /// model focuses on producing a summary rather than calling tools.
    async fn run_single_turn(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        mode: AgentMode,
    ) -> anyhow::Result<String> {
        let (text, _, _) = self.stream_one_turn(tx, mode, false).await?;
        Ok(text)
    }

    /// Estimate the token overhead for items sent with every request but NOT
    /// stored in `session.messages`: tool schemas and the dynamic context block.
    fn estimate_schema_overhead(&self, mode: AgentMode) -> usize {
        let schema_tokens: usize = self.tools.schemas_for_mode(mode)
            .iter()
            .map(|s| (s.name.len() + s.description.len() + s.parameters.to_string().len()) / 4)
            .sum();
        let dynamic_tokens = self.dynamic_context()
            .map(|s| s.len() / 4)
            .unwrap_or(0);
        schema_tokens + dynamic_tokens
    }

    /// Single compaction entry point.  Checks the effective token budget and
    /// compacts the session if needed.  Called before every model submission
    /// (pre-submit at `turn=0`) and after every batch of tool results during
    /// the agentic loop (at the current `turn` number).
    ///
    /// Three compaction paths:
    /// - **Normal**: rolling LLM-based compaction (structured or narrative).
    /// - **Emergency**: session too large for a compaction prompt; drops old
    ///   messages without a model call to guarantee recovery.
    /// - **No-op**: effective token count is below the trigger threshold.
    async fn ensure_fits_budget(
        &mut self,
        tx: &mpsc::Sender<AgentEvent>,
        mode: AgentMode,
        turn: u32,
    ) -> anyhow::Result<()> {
        let input_budget = self.session.input_budget();
        if input_budget == 0 {
            return Ok(());
        }

        // Effective threshold accounts for the overhead reserve so compaction
        // fires before the hard ceiling is reached.
        let threshold = self.config.compaction_threshold - self.config.compaction_overhead_reserve;
        let threshold = threshold.max(0.1); // never below 10%

        if !self.session.is_near_limit(threshold) {
            return Ok(());
        }

        let tokens_before = self.session.token_count;
        let sys = self.system_message(mode);
        let keep_n = self.config.compaction_keep_recent;

        // Emergency path: session so large that even the compaction prompt
        // would overflow the context window.  Drop old messages without a
        // model call — always succeeds regardless of session size.
        let emergency_fraction = 0.95_f32;
        let strategy_used = if self.session.context_fraction() >= emergency_fraction {
            emergency_compact(&mut self.session.messages, Some(sys), keep_n);
            self.session.recalculate_tokens();
            CompactionStrategyUsed::Emergency
        } else {
            // Normal rolling compaction: preserve the recent tail verbatim,
            // summarise everything older.
            let non_system: Vec<Message> = self.session.messages.iter()
                .filter(|m| m.role != Role::System)
                .cloned()
                .collect();

            // Snapshot the original messages so we can restore them if the
            // compaction model call fails (network error, rate limit, etc.).
            // Without this, a failed run_single_turn would leave the session
            // in a partially-compacted state with the original history gone.
            let original_messages = self.session.messages.clone();
            let original_token_count = self.session.token_count;

            // Only use rolling strategy when there are enough messages to make
            // it worthwhile; otherwise summarise the full history.
            let preserve_count = if non_system.len() > keep_n * 2 { keep_n } else { 0 };
            let mut summarize_count = non_system.len().saturating_sub(preserve_count);

            // Safety: adjust the split point backward until `recent_messages`
            // begins at a conversation-turn boundary.  If the split falls
            // inside a tool-use/tool-result group (i.e. `recent_messages[0]`
            // would be a ToolResult or ToolCall), the compacted session would
            // contain orphaned ToolResult blocks — references to ToolCall IDs
            // that were summarised away — causing providers like Anthropic to
            // reject the next request with a 400 error.
            //
            // Moving backward past both ToolResult and ToolCall variants
            // ensures that the entire tool-interaction group (all ToolCall
            // messages AND all their corresponding ToolResult messages) is
            // kept intact in `recent_messages`.
            while summarize_count > 0 && summarize_count < non_system.len() {
                match &non_system[summarize_count].content {
                    MessageContent::ToolResult { .. } | MessageContent::ToolCall { .. } => {
                        summarize_count -= 1;
                    }
                    _ => break,
                }
            }

            let recent_messages: Vec<Message> = non_system[summarize_count..].to_vec();
            let mut to_compact: Vec<Message> = non_system[..summarize_count].to_vec();

            compact_session_with_strategy(
                &mut to_compact,
                Some(sys.clone()),
                &self.config.compaction_strategy,
            );
            self.session.messages = to_compact;
            self.session.recalculate_tokens();

            match self.run_single_turn(tx.clone(), mode).await {
                Ok(summary) if !summary.is_empty() => {
                    // Rebuild: system → summary → preserved recent messages.
                    self.session.messages.clear();
                    self.session.messages.push(sys);
                    self.session.messages.push(Message::assistant(summary));
                    self.session.messages.extend(recent_messages);
                    self.session.recalculate_tokens();

                    match self.config.compaction_strategy {
                        CompactionStrategy::Structured => CompactionStrategyUsed::Structured,
                        CompactionStrategy::Narrative => CompactionStrategyUsed::Narrative,
                    }
                }
                outcome => {
                    // The compaction model call failed or returned an empty
                    // summary.  Restore the original messages so the session
                    // is not left in a corrupt partial-compaction state, then
                    // fall back to the deterministic emergency path which never
                    // makes a model call and always succeeds.
                    if let Err(ref e) = outcome {
                        warn!("compaction model call failed, falling back to emergency compact: {e}");
                    } else {
                        warn!("compaction returned empty summary, falling back to emergency compact");
                    }
                    self.session.messages = original_messages;
                    self.session.token_count = original_token_count;

                    emergency_compact(&mut self.session.messages, Some(sys), keep_n);
                    self.session.recalculate_tokens();
                    CompactionStrategyUsed::Emergency
                }
            }
        };

        let _ = tx.send(AgentEvent::ContextCompacted {
            tokens_before,
            tokens_after: self.session.token_count,
            strategy: strategy_used,
            turn,
        }).await;

        Ok(())
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
    /// Finalise the accumulated arguments buffer into a [`ToolCall`].
    ///
    /// `output_was_truncated` is `true` when the provider signalled that the
    /// model hit its output-token limit during this turn.  For `fs` write/append
    /// calls this enables partial-content recovery: whatever the model managed
    /// to generate before the cut-off is extracted and marked with
    /// `__truncated: true` so the tool can write the partial content and guide
    /// the model to continue with an `append` call.
    fn finish(self, output_was_truncated: bool) -> ToolCall {
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
                Err(parse_err) => {
                    // When the output was truncated and this is an fs write/append
                    // call, try to salvage the partial content that was generated
                    // before the cut-off so the tool can do a partial write and
                    // guide the model to continue with append.
                    if output_was_truncated && (self.name == "fs") {
                        if let Some(v) = extract_partial_fs_args(&self.args_buf) {
                            warn!(
                                tool_name = %self.name,
                                tool_call_id = %self.id,
                                "recovered partial fs arguments from truncated output"
                            );
                            return ToolCall { id: self.id, name: self.name, args: v };
                        }
                    }
                    // Attempt generic JSON repairs before giving up.
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
                                error = %parse_err,
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

/// For a truncated `fs` write/append call, extract the operation, path, and
/// whatever partial `content` the model generated before the output was cut
/// off.  Returns a JSON object with `__truncated: true` so the tool can write
/// the partial content and instruct the model to continue with `append`.
///
/// The function handles all JSON string escape sequences so that content
/// containing `\n`, `\"`, unicode escapes, etc. is decoded correctly.
fn extract_partial_fs_args(json_str: &str) -> Option<serde_json::Value> {
    // Locate the "content" key's value opening quote.
    // Accept both `"content":"` and `"content": "` (with optional whitespace).
    let content_key_pos = json_str.find("\"content\"")?;
    let after_key = json_str[content_key_pos + "\"content\"".len()..].trim_start();
    let after_colon = after_key.strip_prefix(':')?.trim_start();
    // Must start the value with a quote.
    let raw_value = after_colon.strip_prefix('"')?;

    // Extract operation and path from the portion before the content key,
    // which is always complete since it comes first in the JSON.
    let before_content = json_str[..content_key_pos].trim_end_matches(',').trim();
    let prefix_obj: serde_json::Value =
        serde_json::from_str(&format!("{}}}", before_content)).ok()?;
    let operation = prefix_obj.get("operation").and_then(|v| v.as_str())?.to_string();
    let path      = prefix_obj.get("path").and_then(|v| v.as_str())?.to_string();

    // Decode the JSON string content that was generated before truncation.
    let partial_content = extract_partial_json_string(raw_value);

    Some(serde_json::json!({
        "operation":   operation,
        "path":        path,
        "content":     partial_content,
        "__truncated": true,
    }))
}

/// Decode a JSON string value that may be cut off anywhere — including in the
/// middle of an escape sequence.  The input is everything *after* the opening
/// `"` of the JSON string, running to the end of the available data.
///
/// Returns the decoded string content up to (and not including) the closing
/// `"` if present, or up to the point of truncation.
fn extract_partial_json_string(raw: &str) -> String {
    let mut result = String::new();
    let mut chars  = raw.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // Clean end of the JSON string.
            '"' => break,
            // Escape sequence.
            '\\' => match chars.next() {
                Some('"')  => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('/')  => result.push('/'),
                Some('n')  => result.push('\n'),
                Some('r')  => result.push('\r'),
                Some('t')  => result.push('\t'),
                Some('b')  => result.push('\x08'),
                Some('f')  => result.push('\x0C'),
                Some('u')  => {
                    // Collect up to 4 hex digits; stop at truncation.
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() == 4 {
                        if let Ok(code) = u16::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(code as u32) {
                                result.push(ch);
                            }
                        }
                    }
                    // Truncated mid-escape: discard the incomplete sequence.
                }
                // Unknown escape or truncation: stop gracefully.
                Some(c) => result.push(c),
                None    => {}
            },
            c => result.push(c),
        }
    }

    result
}

#[cfg(test)]
mod partial_write_tests {
    use super::*;

    // ── extract_partial_json_string ───────────────────────────────────────────

    #[test]
    fn full_string_with_closing_quote() {
        assert_eq!(extract_partial_json_string(r#"hello world""#), "hello world");
    }

    #[test]
    fn truncated_plain_string() {
        assert_eq!(extract_partial_json_string("no closing quote"), "no closing quote");
    }

    #[test]
    fn newline_and_tab_escapes() {
        // JSON: "line1\nline2\ttabbed"
        assert_eq!(extract_partial_json_string("line1\\nline2\\ttabbed\""), "line1\nline2\ttabbed");
    }

    #[test]
    fn escaped_double_quotes_inside_value() {
        // JSON: "say \"hi\""
        assert_eq!(extract_partial_json_string("say \\\"hi\\\"\""), "say \"hi\"");
    }

    #[test]
    fn truncated_mid_escape_sequence() {
        // Truncated right after the backslash — the incomplete escape is dropped.
        let result = extract_partial_json_string("partial\\");
        assert_eq!(result, "partial");
    }

    #[test]
    fn unicode_escape() {
        // JSON: "\u0041"
        assert_eq!(extract_partial_json_string("\\u0041\""), "A");
    }

    // ── extract_partial_fs_args ───────────────────────────────────────────────

    #[test]
    fn recovers_write_with_complete_content() {
        let json = r#"{"operation":"write","path":"/tmp/foo.md","content":"hello world"}"#;
        // Full parse should succeed normally; this tests the fallback path isn't needed.
        let result = extract_partial_fs_args(json);
        // Should still work via this function.
        let v = result.unwrap();
        assert_eq!(v["operation"], "write");
        assert_eq!(v["path"], "/tmp/foo.md");
        assert_eq!(v["content"], "hello world");
        assert_eq!(v["__truncated"], true);
    }

    #[test]
    fn recovers_write_truncated_right_at_opening_quote() {
        // Truncated right after the opening `"` of the content value — empty partial.
        let json = r#"{"operation":"write","path":"/tmp/foo.md","content":""#;
        let v = extract_partial_fs_args(json).unwrap();
        assert_eq!(v["operation"], "write");
        assert_eq!(v["path"], "/tmp/foo.md");
        assert_eq!(v["content"], "");
        assert_eq!(v["__truncated"], true);
    }

    #[test]
    fn recovers_write_with_partial_content() {
        // JSON args truncated mid-content: {"operation":"write","path":"...","content":"# Title\n\nSome partial
        let json = "{\"operation\":\"write\",\"path\":\"/tmp/foo.md\",\"content\":\"# Title\\n\\nSome partial";
        let v = extract_partial_fs_args(json).unwrap();
        assert_eq!(v["operation"], "write");
        assert_eq!(v["content"], "# Title\n\nSome partial");
        assert_eq!(v["__truncated"], true);
    }

    #[test]
    fn recovers_write_with_escaped_quotes_in_content() {
        // JSON args: {"operation":"write","path":"...","content":"She said \"hello  (truncated)
        let json = "{\"operation\":\"write\",\"path\":\"/tmp/foo.md\",\"content\":\"She said \\\"hello";
        let v = extract_partial_fs_args(json).unwrap();
        assert_eq!(v["content"], "She said \"hello");
        assert_eq!(v["__truncated"], true);
    }

    #[test]
    fn recovers_append_operation() {
        let json = r#"{"operation":"append","path":"/tmp/foo.md","content":"more text"#;
        let v = extract_partial_fs_args(json).unwrap();
        assert_eq!(v["operation"], "append");
        assert_eq!(v["content"], "more text");
    }

    #[test]
    fn returns_none_when_no_content_key() {
        let json = r#"{"operation":"write","path":"/tmp/foo.md"}"#;
        // No content key at all — nothing to recover.
        assert!(extract_partial_fs_args(json).is_none());
    }

    #[test]
    fn returns_none_when_no_path() {
        let json = r#"{"operation":"write","content":"data"#;
        assert!(extract_partial_fs_args(json).is_none());
    }
}
