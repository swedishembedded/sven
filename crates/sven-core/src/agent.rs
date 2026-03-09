// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use sven_config::{AgentConfig, AgentMode, CompactionStrategy};
use sven_model::{CompletionRequest, FunctionCall, Message, MessageContent, ResponseEvent, Role};
use sven_tools::{events::ToolEvent, ToolCall, ToolRegistry};

use crate::{
    compact::{compact_session_with_strategy, emergency_compact, smart_truncate},
    events::{AgentEvent, CompactionStrategyUsed},
    prompts::system_prompt,
    runtime_context::AgentRuntimeContext,
    session::Session,
    tool_slots::ToolSlotManager,
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
        let max_output_tokens = model
            .config_max_output_tokens()
            .or_else(|| model.catalog_max_output_tokens())
            .unwrap_or(0) as usize;
        let mut session = Session::new(max_context_tokens);
        session.max_output_tokens = max_output_tokens;
        // Pre-populate with prior messages if provided (used by session executor
        // to restore conversation context from the local conversation store).
        if !runtime.prior_messages.is_empty() {
            session.push_many(runtime.prior_messages.iter().cloned());
        }
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

    /// Expose the shared mode lock so external callers (e.g. ACP mode-switch
    /// requests) can update the mode without going through the tool event channel.
    pub fn current_mode_lock(&self) -> &Arc<tokio::sync::Mutex<sven_config::AgentMode>> {
        &self.current_mode
    }

    /// Used by the CI runner to switch models mid-workflow (per-step model
    /// overrides).  The session history is preserved.
    pub fn set_model(&mut self, model: Arc<dyn sven_model::ModelProvider>) {
        // Update context window: prefer config-specified total, then catalog.
        if let Some(cw) = model
            .config_context_window()
            .or_else(|| model.catalog_context_window())
        {
            self.session.max_tokens = cw as usize;
        }
        // Update output token limit: prefer config-specified, then catalog.
        if let Some(mot) = model
            .config_max_output_tokens()
            .or_else(|| model.catalog_max_output_tokens())
        {
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
            let _ = tx
                .send(AgentEvent::Aborted {
                    partial_text: String::new(),
                })
                .await;
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
            let _ = tx
                .send(AgentEvent::Aborted {
                    partial_text: String::new(),
                })
                .await;
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
        let mut empty_turn_retries = 0u32;
        const MAX_EMPTY_TURN_RETRIES: u32 = 2;

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
                // with_tools=false so slot_manager is always empty; discard it.
                if let Some(Ok((text, _slot_manager, _))) = wrap_turn {
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

            let (text, slot_manager, had_tool_calls) = match turn {
                None => {
                    // Aborted mid-stream.  Any tool slots dispatched during
                    // streaming are aborted via ToolSlotManager::Drop.
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
                if text.is_empty() && empty_turn_retries < MAX_EMPTY_TURN_RETRIES {
                    empty_turn_retries += 1;
                    self.session.push(Message::user(
                        "You produced a thinking block but no response or tool call. \
                         Please continue with your next action.",
                    ));
                    continue;
                }
                if !text.is_empty()
                    && text_contains_malformed_tool_call(&text)
                    && empty_turn_retries < MAX_EMPTY_TURN_RETRIES
                {
                    empty_turn_retries += 1;
                    self.session.push(Message::user(
                        "You output a tool call using an incorrect format (XML/function tags \
                         in the text response). Do not include tool calls in your text. \
                         Use the JSON tool-call protocol provided by your schema.",
                    ));
                    continue;
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            empty_turn_retries = 0;

            // Await tool results with cancellation support.
            //
            // slot_manager is moved into the join_all future.  If cancel fires
            // during execution the future is dropped, triggering
            // ToolSlotManager::Drop which aborts every in-flight task.
            //
            // A 100 ms drain timer interleaves with join_all so that progress
            // events and mode-change side-effects reach the UI and the agent
            // state while tools are running.
            let join_fut = slot_manager.join_all(&tx);
            tokio::pin!(join_fut);
            let results = loop {
                tokio::select! {
                    biased;
                    _ = &mut *cancel => {
                        // join_fut is dropped when we return; ToolSlotManager
                        // Drop aborts all in-flight handles automatically.
                        let _ = tx
                            .send(AgentEvent::Aborted { partial_text })
                            .await;
                        return Ok(());
                    }
                    res = &mut join_fut => break res,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        self.drain_tool_events(&tx).await;
                    }
                }
            };
            // Final drain after all tools complete.
            self.drain_tool_events(&tx).await;

            // Push all assistant ToolCall messages first (OpenAI wire format).
            for (tc, _) in &results {
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

            // Push all ToolResult messages with smart truncation.
            let cap = self.config.tool_result_token_cap;
            for (tc, output) in &results {
                let category = self.tools.output_category(&tc.name);
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output
                        .parts
                        .iter()
                        .map(|p| match p {
                            sven_tools::ToolOutputPart::Text(t) => {
                                let truncated = smart_truncate(t, category, cap);
                                ToolContentPart::Text { text: truncated }
                            }
                            sven_tools::ToolOutputPart::Image(url) => ToolContentPart::Image {
                                image_url: url.clone(),
                            },
                        })
                        .collect();
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
        let mut empty_turn_retries = 0u32;
        const MAX_EMPTY_TURN_RETRIES: u32 = 2;

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
                // with_tools=false so slot_manager is always empty; discard it.
                let (text, _slot_manager, _) =
                    self.stream_one_turn(tx.clone(), mode, false).await?;
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
            let (text, slot_manager, had_tool_calls) =
                self.stream_one_turn(tx.clone(), mode, true).await?;

            if !text.is_empty() {
                self.session.push(Message::assistant(&text));
            }

            if !had_tool_calls {
                if text.is_empty() && empty_turn_retries < MAX_EMPTY_TURN_RETRIES {
                    empty_turn_retries += 1;
                    self.session.push(Message::user(
                        "You produced a thinking block but no response or tool call. \
                         Please continue with your next action.",
                    ));
                    continue;
                }
                // Detect XML / Hermes-style tool call syntax written into the text
                // stream.  Some models emit <tool_call>...</tool_call> as plain text
                // instead of using the JSON tool-call protocol.  Push a correction so
                // the model retries in the correct format rather than wasting the turn.
                if !text.is_empty()
                    && text_contains_malformed_tool_call(&text)
                    && empty_turn_retries < MAX_EMPTY_TURN_RETRIES
                {
                    empty_turn_retries += 1;
                    self.session.push(Message::user(
                        "You output a tool call using an incorrect format (XML/function tags \
                         in the text response). Do not include tool calls in your text. \
                         Use the JSON tool-call protocol provided by your schema.",
                    ));
                    continue;
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            empty_turn_retries = 0;

            // Await all tool results.  ToolCallStarted events were already
            // emitted during streaming (in stream_one_turn) as each slot's
            // args became complete.  join_all emits ToolCallFinished for each
            // slot as it completes (in any order).
            //
            // A 100 ms drain timer interleaves with join_all so that progress
            // events and mode-change side-effects (session patching) reach the
            // UI and the agent state without waiting for the slowest tool.
            let join_fut = slot_manager.join_all(&tx);
            tokio::pin!(join_fut);
            let results = loop {
                tokio::select! {
                    res = &mut join_fut => break res,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        self.drain_tool_events(&tx).await;
                    }
                }
            };
            // Final drain after all tools complete.
            self.drain_tool_events(&tx).await;

            // Push all assistant ToolCall messages first (OpenAI wire format
            // requires all ToolCall messages to precede any ToolResult messages).
            for (tc, _) in &results {
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

            // Push all ToolResult messages, applying smart truncation when a
            // result exceeds the configured token cap.
            let cap = self.config.tool_result_token_cap;
            for (tc, output) in &results {
                let category = self.tools.output_category(&tc.name);
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output
                        .parts
                        .iter()
                        .map(|p| match p {
                            sven_tools::ToolOutputPart::Text(t) => {
                                let truncated = smart_truncate(t, category, cap);
                                ToolContentPart::Text { text: truncated }
                            }
                            sven_tools::ToolOutputPart::Image(url) => ToolContentPart::Image {
                                image_url: url.clone(),
                            },
                        })
                        .collect();
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
                    // Patch the system message in-place so every subsequent
                    // model call within the same turn sees the new mode
                    // instructions and consistent tool set.  This still busts
                    // the provider's prompt cache (the prefix changed), but
                    // avoids sending a contradictory "Operating Mode: plan"
                    // header while the agent-mode tool set is active.
                    let new_sys = self.system_message(new_mode);
                    if let Some(first) = self.session.messages.first_mut() {
                        if first.role == sven_model::Role::System {
                            *first = new_sys;
                        }
                    }
                    let _ = tx.send(AgentEvent::ModeChanged(new_mode)).await;
                }
                ToolEvent::Progress { call_id, message } => {
                    let _ = tx.send(AgentEvent::ToolProgress { call_id, message }).await;
                }
                ToolEvent::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                } => {
                    // Forward as a CollabEvent so the TUI can render a
                    // collapsible DelegateSummary segment.
                    let ev = crate::prompts::CollabEvent::TeammateFinished {
                        name: to_name.clone(),
                        task_id: String::new(),
                        status: status.clone(),
                    };
                    let _ = tx.send(AgentEvent::CollabEvent(ev)).await;
                    // Also forward the structured delegate summary.
                    let _ = tx
                        .send(AgentEvent::DelegateSummary {
                            to_name,
                            task_title,
                            duration_ms,
                            status,
                            result_preview,
                        })
                        .await;
                }
            }
        }
    }

    /// Call the model once, streaming text deltas and dispatching tool calls
    /// as soon as their JSON arguments are complete.
    ///
    /// Returns `(full_text, slot_manager, had_tool_calls)`.
    ///
    /// `slot_manager` holds one `JoinHandle` per dispatched tool.  All
    /// `ToolCallStarted` events are emitted here (during streaming).  The
    /// caller must call [`ToolSlotManager::join_all`] to await results and
    /// emit the corresponding `ToolCallFinished` events.
    async fn stream_one_turn(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        mode: AgentMode,
        with_tools: bool,
    ) -> anyhow::Result<(String, ToolSlotManager, bool)> {
        let tools: Vec<sven_model::ToolSchema> = if with_tools {
            self.tools
                .schemas_for_mode(mode)
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
            max_output_tokens_override: None,
        };

        let mut stream = match self.model.complete(req).await {
            Ok(s) => s,
            Err(e) => {
                // When the provider reports a hard context-size overflow
                // (e.g. llama.cpp `exceed_context_size_error` with `n_ctx`),
                // update the session budget to the actual value, compact, and
                // retry once.  This handles the case where the catalog or config
                // context window is larger than what the server was loaded with.
                if let Some(n_ctx) = extract_n_ctx_from_error(&e) {
                    warn!(
                        n_ctx,
                        old_max_tokens = self.session.max_tokens,
                        "context overflow: catalog/config budget was wrong; \
                         updating to actual n_ctx and compacting before retry"
                    );
                    // Update the session budget to the real server value so all
                    // subsequent ensure_fits_budget calls use the correct ceiling
                    // and will prefer LLM summarization going forward.
                    self.session.max_tokens = n_ctx;
                    // Use a direct emergency compact here rather than calling
                    // ensure_fits_budget: ensure_fits_budget drives a LLM
                    // summarization turn through run_single_turn → stream_one_turn,
                    // which would create an unresolvable async recursion cycle.
                    // Emergency compact is the safe recovery primitive; LLM-based
                    // summarization will apply correctly on the next proactive
                    // compaction check now that max_tokens reflects the real limit.
                    let sys = self.system_message(mode);
                    emergency_compact(
                        &mut self.session.messages,
                        Some(sys),
                        self.config.compaction_keep_recent,
                    );
                    self.session.recalculate_tokens();
                    // Rebuild request with the compacted message set.
                    let modalities2 = self.model.input_modalities();
                    let messages2 = sven_model::sanitize::strip_images_if_unsupported(
                        self.session.messages.clone(),
                        &modalities2,
                    );
                    let req2 = CompletionRequest {
                        messages: messages2,
                        tools: tools.clone(),
                        stream: true,
                        system_dynamic_suffix: self.dynamic_context(),
                        cache_key: Some(self.session.id.clone()),
                        max_output_tokens_override: None,
                    };
                    self.model
                        .complete(req2)
                        .await
                        .context("model completion failed (after context recovery)")?
                } else {
                    return Err(e).context("model completion failed");
                }
            }
        };

        let mut full_text = String::new();
        // Manages per-slot parallel dispatch: slots are started as soon as
        // their JSON arguments form a valid object during streaming.
        let mut slot_manager = ToolSlotManager::new(Arc::clone(&self.tools));
        // Accumulate thinking deltas so we can emit a single ThinkingComplete
        // event to consumers (CI runner, TUI) once the thinking block ends.
        let mut thinking_buf = String::new();

        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::MaxTokens => {}
                ResponseEvent::ThinkingDelta(delta) => {
                    thinking_buf.push_str(&delta);
                    let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
                }
                ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                    // Flush accumulated thinking when text starts arriving.
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx
                            .send(AgentEvent::ThinkingComplete(strip_think_wrappers(content)))
                            .await;
                    }
                    full_text.push_str(&delta);
                    let _ = tx.send(AgentEvent::TextDelta(delta)).await;
                }
                ResponseEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    // Dispatch the slot immediately when its args form valid
                    // JSON; emit ToolCallStarted so the UI shows progress.
                    if let Some(tc) = slot_manager.feed(index, &id, &name, &arguments) {
                        let _ = tx.send(AgentEvent::ToolCallStarted(tc)).await;
                    }
                }
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                } => {
                    self.session
                        .add_cache_usage(cache_read_tokens, cache_write_tokens);
                    // Update the running calibration factor using the provider's
                    // actual input token count.  This corrects the chars/4
                    // approximation for the current workload and model.
                    let actual_input = input_tokens + cache_read_tokens;
                    if actual_input > 0 {
                        let estimated = self.session.token_count + self.session.schema_overhead;
                        self.session.update_calibration(actual_input, estimated);
                    }
                    let _ = tx
                        .send(AgentEvent::TokenUsage {
                            input: input_tokens,
                            output: output_tokens,
                            cache_read: cache_read_tokens,
                            cache_write: cache_write_tokens,
                            cache_read_total: self.session.cache_read_total,
                            cache_write_total: self.session.cache_write_total,
                            max_tokens: self.session.max_tokens,
                            max_output_tokens: self.session.max_output_tokens,
                        })
                        .await;
                }
                ResponseEvent::Done => {
                    // Flush any trailing thinking block (model thought without responding).
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx
                            .send(AgentEvent::ThinkingComplete(strip_think_wrappers(content)))
                            .await;
                    }
                    break;
                }
                ResponseEvent::Error(e) => {
                    warn!("model stream error: {e}");
                }
                _ => {}
            }

            // Drain tool events produced by already-dispatched slots so that
            // progress updates reach the UI while the LLM stream continues.
            self.drain_tool_events(&tx).await;
        }

        // When a model that doesn't use reasoning_content (e.g. a local GGUF
        // served without reasoning_format: deepseek) emits its thinking as
        // plain <think>...</think> text, full_text ends up containing the tag
        // wrapper but no real response.  Detect this: if the entire text output
        // is a single <think>...</think> block (possibly unclosed if the model
        // truncated), reclassify it as thinking and clear full_text so the
        // agent loop correctly sees a thinking-only turn and applies the retry.
        if !full_text.is_empty() && thinking_buf.is_empty() {
            if let Some(inline_think) = extract_inline_think_block(&full_text) {
                let _ = tx.send(AgentEvent::ThinkingComplete(inline_think)).await;
                full_text.clear();
            }
        }

        // Finalize any slots whose JSON args were still incomplete when the
        // stream ended (truncated output, slow streaming, etc.).
        // Tool calls with an empty name cannot be dispatched and are dropped —
        // storing them would corrupt the conversation history sent back to the
        // API on the next turn.
        for tc in slot_manager.finalize_remaining() {
            let _ = tx.send(AgentEvent::ToolCallStarted(tc)).await;
        }

        // If no structured tool calls arrived but the text contains Anthropic-style
        // <invoke> markup (MiniMax and other models that fall back to the old XML
        // function-call format), extract them so they are executed as real tool calls
        // rather than being silently discarded.  The invoke blocks are stripped from
        // full_text so the session history stays clean.
        if slot_manager.is_empty() && full_text.contains("<invoke ") {
            let (cleaned, invoke_calls) = extract_inline_invoke_tool_calls(&full_text);
            if !invoke_calls.is_empty() {
                warn!(
                    count = invoke_calls.len(),
                    "model emitted Anthropic-style <invoke> tool calls in text; \
                     extracting for execution"
                );
                full_text = cleaned;
                for (i, tc) in invoke_calls.into_iter().enumerate() {
                    let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                    slot_manager.insert_call(i as u32, tc);
                }
            }
        }

        if !full_text.is_empty() {
            let _ = tx.send(AgentEvent::TextComplete(full_text.clone())).await;
        }

        let had_tool_calls = !slot_manager.is_empty();
        Ok((full_text, slot_manager, had_tool_calls))
    }

    /// Run a single tool-free turn and return the full text response.
    /// Used for compaction summary generation; no tools are passed so the
    /// model focuses on producing a summary rather than calling tools.
    async fn run_single_turn(
        &mut self,
        tx: mpsc::Sender<AgentEvent>,
        mode: AgentMode,
    ) -> anyhow::Result<String> {
        // with_tools=false → slot_manager is always empty; drop it immediately.
        let (text, _slot_manager, _) = self.stream_one_turn(tx, mode, false).await?;
        Ok(text)
    }

    /// Estimate the token overhead for items sent with every request but NOT
    /// stored in `session.messages`: tool schemas and the dynamic context block.
    fn estimate_schema_overhead(&self, mode: AgentMode) -> usize {
        let schema_tokens: usize = self
            .tools
            .schemas_for_mode(mode)
            .iter()
            .map(|s| (s.name.len() + s.description.len() + s.parameters.to_string().len()) / 4)
            .sum();
        let dynamic_tokens = self.dynamic_context().map(|s| s.len() / 4).unwrap_or(0);
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

        // Pre-compute the message split so the emergency decision can be based
        // on whether the compaction prompt (old messages only) fits within the
        // budget — not whether the full session fits.  The compaction call only
        // sends `to_compact` to the model, so checking the full session is
        // unnecessarily pessimistic: it would force information-destroying
        // emergency drops even when the old messages alone are well within the
        // window.
        let non_system: Vec<Message> = self
            .session
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .cloned()
            .collect();

        let preserve_count = if non_system.len() > keep_n * 2 {
            keep_n
        } else {
            0
        };
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

        // Emergency check: would the compaction prompt itself exceed the budget?
        //
        // The compaction call sends only the OLD messages (to_compact), not the
        // recent tail.  Estimate the compaction prompt size by subtracting the
        // recent-tail token count from the total tracked session token count.
        // Using the session's own token accounting (rather than recomputing with
        // the freshly generated system prompt) keeps this consistent with the
        // calibration factor and with how test sessions are seeded.
        //
        // If even the old-messages portion of the session fills 95 % of the
        // budget, there is not enough space left for the model to emit a summary.
        let recent_raw_tokens: usize = non_system[summarize_count..]
            .iter()
            .map(|m| m.approx_tokens())
            .sum();
        let compaction_input_raw = self.session.token_count.saturating_sub(recent_raw_tokens);
        let calibrated_compaction_input =
            (compaction_input_raw as f32 * self.session.calibration_factor) as usize;
        let emergency_fraction = 0.95_f32;
        let compaction_would_overflow = summarize_count == 0
            || (calibrated_compaction_input as f32 / input_budget as f32) >= emergency_fraction;

        let strategy_used = if compaction_would_overflow {
            // Emergency path: even the compaction call would overflow, or there
            // is nothing to summarize.  Drop old messages without a model call —
            // always succeeds regardless of session size.
            emergency_compact(&mut self.session.messages, Some(sys), keep_n);
            self.session.recalculate_tokens();
            CompactionStrategyUsed::Emergency
        } else {
            // Normal rolling compaction: preserve the recent tail verbatim,
            // summarise everything older.
            // Snapshot the original messages so we can restore them if the
            // compaction model call fails (network error, rate limit, etc.).
            // Without this, a failed run_single_turn would leave the session
            // in a partially-compacted state with the original history gone.
            let original_messages = self.session.messages.clone();
            let original_token_count = self.session.token_count;

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
                        warn!(
                            "compaction model call failed, falling back to emergency compact: {e}"
                        );
                    } else {
                        warn!(
                            "compaction returned empty summary, falling back to emergency compact"
                        );
                    }
                    self.session.messages = original_messages;
                    self.session.token_count = original_token_count;

                    emergency_compact(&mut self.session.messages, Some(sys), keep_n);
                    self.session.recalculate_tokens();
                    CompactionStrategyUsed::Emergency
                }
            }
        };

        let _ = tx
            .send(AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after: self.session.token_count,
                strategy: strategy_used,
                turn,
            })
            .await;

        Ok(())
    }

    /// Returns the system message that will be (or was) used for `mode`.
    ///
    /// Callers can persist this to a JSONL log so that resumed conversations
    /// replay with exactly the same prompt.
    pub fn current_system_message(&self, mode: AgentMode) -> Message {
        self.system_message(mode)
    }

    fn system_message(&self, mode: AgentMode) -> Message {
        let ctx = self.prompt_context();
        // Use the STABLE portion only — volatile context (git/CI) is injected
        // per-request via `system_dynamic_suffix` so it does not break prompt
        // caching across sessions.
        let stable_ctx = ctx.stable_only();
        let custom = self
            .runtime
            .system_prompt_override
            .as_deref()
            .or(self.config.system_prompt.as_deref());
        Message::system(system_prompt(mode, custom, stable_ctx))
    }

    /// Build a `PromptContext` from the current runtime environment.
    fn prompt_context(&self) -> crate::prompts::PromptContext<'_> {
        crate::prompts::PromptContext {
            project_root: self.runtime.project_root.as_deref(),
            git_context: self.runtime.git_context_note.as_deref(),
            project_context_file: self.runtime.project_context_file.as_deref(),
            ci_context: self.runtime.ci_context_note.as_deref(),
            append: self.runtime.append_system_prompt.as_deref(),
            skills: self.runtime.skills.get(),
            agents: self.runtime.agents.get(),
            knowledge: self.runtime.knowledge.get(),
            knowledge_drift_note: self.runtime.knowledge_drift_note.as_deref(),
        }
    }

    /// Volatile context (git + CI) formatted for injection as an uncached
    /// system block.  Returns `None` when no dynamic context is configured.
    fn dynamic_context(&self) -> Option<String> {
        // When a custom system prompt override is in use, the caller controls
        // all content — skip the dynamic injection to avoid duplication.
        if self.runtime.system_prompt_override.is_some() || self.config.system_prompt.is_some() {
            return None;
        }
        self.prompt_context().dynamic_block()
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

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

/// Try to extract `n_ctx` from a context-overflow API error.
///
/// llama.cpp-compatible backends return a structured error body when the
/// request exceeds the loaded context window:
///
/// ```json
/// {"error":{"type":"exceed_context_size_error","n_ctx":54272,"n_prompt_tokens":54298,...}}
/// ```
///
/// Returns `Some(n_ctx)` when the error message contains that pattern,
/// `None` for any other error.
fn extract_n_ctx_from_error(err: &anyhow::Error) -> Option<usize> {
    let msg = err.to_string();
    if !msg.contains("exceed_context_size_error") {
        return None;
    }
    // The error string is "<driver> error <status>: <json-body>".
    // Find the first '{' and try to parse the JSON fragment from there.
    let json_start = msg.find('{')?;
    let body: serde_json::Value = serde_json::from_str(&msg[json_start..]).ok()?;
    // {"error": {"n_ctx": …}}
    if let Some(n) = body["error"]["n_ctx"].as_u64() {
        return Some(n as usize);
    }
    // Flat format: {"n_ctx": …}
    body["n_ctx"].as_u64().map(|n| n as usize)
}

/// Strip `<think>` / `</think>` wrapper tags from accumulated thinking content.
///
/// Some model servers (llama.cpp without `reasoning_format: deepseek`,
/// certain OpenAI-compat proxies) forget to strip these tags before placing
/// the text in `reasoning_content`.  The result is that the thinking buffer
/// contains the raw markup, e.g. `<think>\nStep 1: …\n</think>`, instead of
/// the clean inner text.  Stripping them here keeps the thinking log readable
/// and prevents the `<think>` noise from leaking into conversation history.
fn strip_think_wrappers(s: String) -> String {
    let trimmed = s.trim();
    let inner = trimmed.strip_prefix("<think>").unwrap_or(trimmed);
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    inner.trim().to_string()
}

/// Detect a `<think>...</think>` block occupying the *entire* text.
///
/// Some models emit thinking as plain text deltas (no `reasoning_content`)
/// when the serving layer isn't configured for reasoning extraction.  If the
/// whole text response is a `<think>` block — with or without a closing tag
/// (the model may have been cut off) — the "response" carries no useful
/// content.  Return the extracted inner text so the caller can reclassify
/// it as thinking and clear `full_text`, which causes the agent loop to
/// treat this as a thinking-only turn and apply the empty-turn retry nudge.
///
/// Returns `None` when the text contains content outside the `<think>` block.
fn extract_inline_think_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Must start with <think>
    let inner = trimmed.strip_prefix("<think>")?;
    // Strip an optional closing tag; an unclosed block (model truncated) is
    // still all-thinking if there is nothing after the last </think>.
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    // Reject if there's a *second* </think> inside, which would mean there's
    // real content after the first block.
    if inner.contains("</think>") {
        return None;
    }
    Some(inner.trim().to_string())
}

/// Return true when `text` contains tool-call markup that was written by the
/// model into the text stream instead of being emitted as a structured tool
/// call.  Some fine-tuned models (Qwen, older Llama variants, MiniMax) fall
/// back to XML-style or Hermes-style function call syntax even when the
/// provider tool-call protocol is available.
///
/// Patterns detected:
/// - `<tool_call>` / `</tool_call>` (Qwen XML format)
/// - `<function=name>` (Hermes/Nous function tag)
/// - `[TOOL_CALL]` (some other open-source variants)
/// - `<invoke ` (Anthropic old XML / MiniMax format — handled by
///   `extract_inline_invoke_tool_calls`; listed here as a safety net)
fn text_contains_malformed_tool_call(text: &str) -> bool {
    text.contains("<tool_call>")
        || text.contains("</tool_call>")
        || text.contains("<function=")
        || text.contains("[TOOL_CALL]")
        || text.contains("<invoke ")
}

/// Extract Anthropic-style `<invoke>` tool calls written inline in the text
/// stream by models (e.g. MiniMax) that fall back to the old XML
/// function-call format instead of using the structured tool-call protocol.
///
/// Format:
/// ```text
/// <invoke name="tool_name">
/// <parameter name="param1">value1</parameter>
/// <parameter name="param2">value2</parameter>
/// </invoke>
/// ```
///
/// Returns the text with all `<invoke>…</invoke>` blocks removed and the
/// extracted [`ToolCall`] objects.  Parameter values that parse as valid JSON
/// are stored as JSON; otherwise they are stored as plain strings.
fn extract_inline_invoke_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    use regex::Regex;

    let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
    let param_re = Regex::new(r#"(?s)<parameter\s+name="([^"]+)">(.*?)</parameter>"#).unwrap();

    let mut tool_calls = Vec::new();

    for cap in invoke_re.captures_iter(text) {
        let name = cap[1].to_string();
        let body = &cap[2];

        let mut args = serde_json::Map::new();
        for param in param_re.captures_iter(body) {
            let key = param[1].to_string();
            let raw = param[2].trim().to_string();
            // Try to decode as JSON (for nested objects/arrays); fall back to
            // a plain string value.
            let val = serde_json::from_str::<serde_json::Value>(&raw)
                .unwrap_or(serde_json::Value::String(raw));
            args.insert(key, val);
        }

        tool_calls.push(ToolCall {
            id: format!("invoke_{}", uuid::Uuid::new_v4().simple()),
            name,
            args: serde_json::Value::Object(args),
        });
    }

    let cleaned = invoke_re.replace_all(text, "").trim().to_string();
    (cleaned, tool_calls)
}
