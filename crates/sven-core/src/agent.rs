use std::sync::Arc;

use anyhow::Context;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

use sven_config::{AgentConfig, AgentMode};
use sven_model::{
    CompletionRequest, FunctionCall, Message, MessageContent, ResponseEvent, Role,
};
use sven_tools::{events::ToolEvent, ToolCall, ToolRegistry};

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

    /// Push a user message, run the agent loop, and stream events through the sender.
    /// The caller drops the receiver when it is no longer interested.
    pub async fn submit(
        &mut self,
        user_input: &str,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let mode = *self.current_mode.lock().await;

        // Proactive compaction before adding the new user message
        if self.session.is_near_limit(self.config.compaction_threshold) {
            let sys = self.system_message(mode);
            let before = compact_session(&mut self.session.messages, Some(sys.clone()));
            self.session.recalculate_tokens();
            let summary = self.run_single_turn(tx.clone(), mode).await?;
            self.session.messages.clear();
            self.session.messages.push(sys);
            self.session.messages.push(Message::assistant(summary.clone()));
            self.session.recalculate_tokens();
            let after = self.session.token_count;
            let _ = tx.send(AgentEvent::ContextCompacted {
                tokens_before: before,
                tokens_after: after,
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

    /// The main agent loop: model call → optional tool calls → repeat
    async fn run_agentic_loop(&mut self, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let mut rounds = 0u32;

        loop {
            rounds += 1;
            if rounds > self.config.max_tool_rounds {
                let _ = tx.send(AgentEvent::Error(format!(
                    "exceeded max tool rounds ({})", self.config.max_tool_rounds
                ))).await;
                break;
            }

            let mode = *self.current_mode.lock().await;
            let (text, tool_calls, had_tool_calls) =
                self.stream_one_turn(tx.clone(), mode).await?;

            if !text.is_empty() {
                self.session.push(Message::assistant(&text));
            }

            if !had_tool_calls {
                let _ = tx.send(AgentEvent::TurnComplete).await;
                break;
            }

            // Execute tool calls and push results
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

                let output = self.tools.execute(tc).await;

                // Drain any tool events emitted during this execution
                self.drain_tool_events(&tx).await;

                let _ = tx.send(AgentEvent::ToolCallFinished {
                    call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    output: output.content.clone(),
                    is_error: output.is_error,
                }).await;

                // Build a tool result message — multipart when the output has images.
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output.parts.iter().map(|p| match p {
                        sven_tools::ToolOutputPart::Text(t) => {
                            ToolContentPart::Text { text: t.clone() }
                        }
                        sven_tools::ToolOutputPart::Image(url) => {
                            ToolContentPart::Image { image_url: url.clone() }
                        }
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
    ) -> anyhow::Result<(String, Vec<ToolCall>, bool)> {
        let tools: Vec<sven_model::ToolSchema> = self.tools.schemas_for_mode(mode)
            .into_iter()
            .map(|s| sven_model::ToolSchema {
                name: s.name,
                description: s.description,
                parameters: s.parameters,
            })
            .collect();

        // Strip image content when the current model does not support images.
        let modalities = self.model.input_modalities();
        let messages = sven_model::sanitize::strip_images_if_unsupported(
            self.session.messages.clone(),
            &modalities,
        );

        let req = CompletionRequest {
            messages,
            tools,
            stream: true,
            // Carry volatile context (git/CI) separately so providers that
            // support prompt caching (Anthropic) can put it in an uncached
            // system block while the stable prefix stays cached.
            system_dynamic_suffix: self.dynamic_context(),
        };

        let mut stream = self.model.complete(req).await
            .context("model completion failed")?;

        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut pending_tc: Option<PendingToolCall> = None;

        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                    full_text.push_str(&delta);
                    let _ = tx.send(AgentEvent::TextDelta(delta)).await;
                }
                ResponseEvent::ToolCall { id, name, arguments } => {
                    if !id.is_empty() {
                        if let Some(ptc) = pending_tc.take() {
                            tool_calls.push(ptc.finish());
                        }
                        pending_tc = Some(PendingToolCall {
                            id,
                            name,
                            args_buf: arguments,
                        });
                    } else if let Some(ptc) = &mut pending_tc {
                        ptc.args_buf.push_str(&arguments);
                        if !name.is_empty() { ptc.name = name; }
                    }
                }
                ResponseEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens } => {
                    let _ = tx.send(AgentEvent::TokenUsage {
                        input: input_tokens,
                        output: output_tokens,
                        context_total: self.session.token_count,
                        cache_read: cache_read_tokens,
                        cache_write: cache_write_tokens,
                    }).await;
                }
                ResponseEvent::Done => break,
                ResponseEvent::Error(e) => {
                    warn!("model stream error: {e}");
                }
                _ => {}
            }
        }

        if let Some(ptc) = pending_tc {
            tool_calls.push(ptc.finish());
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
        let (text, _, _) = self.stream_one_turn(tx, mode).await?;
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
        let args = serde_json::from_str(&self.args_buf).unwrap_or(serde_json::Value::Null);
        ToolCall { id: self.id, name: self.name, args }
    }
}
