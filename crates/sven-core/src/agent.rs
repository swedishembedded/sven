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

                self.session.push(Message::tool_result(&tc.id, &output.content));
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

        let req = CompletionRequest {
            messages: self.session.messages.clone(),
            tools,
            stream: true,
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
                ResponseEvent::Usage { input_tokens, output_tokens } => {
                    let _ = tx.send(AgentEvent::TokenUsage {
                        input: input_tokens,
                        output: output_tokens,
                        context_total: self.session.token_count,
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
        let ctx = crate::prompts::PromptContext {
            project_root: self.runtime.project_root.as_deref(),
            git_context: self.runtime.git_context_note.as_deref(),
            project_context_file: self.runtime.project_context_file.as_deref(),
            ci_context: self.runtime.ci_context_note.as_deref(),
            append: self.runtime.append_system_prompt.as_deref(),
        };
        // Runtime override takes priority over the config-file system_prompt.
        let custom = self.runtime.system_prompt_override.as_deref()
            .or(self.config.system_prompt.as_deref());
        Message::system(system_prompt(
            mode,
            custom,
            &self.tools.names_for_mode(mode),
            ctx,
        ))
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
