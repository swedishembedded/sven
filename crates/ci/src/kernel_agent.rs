// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Kernel-backed drop-in for the small slice of the legacy `sven_machines::Agent`
//! API that the headless runners ([`CiRunner`](crate::CiRunner) and
//! [`ConversationRunner`](crate::ConversationRunner)) depend on.
//!
//! Instead of driving the retired `sven_machines::Agent` loop, [`KernelAgent`]
//! runs each turn on the HSM kernel built by [`RuntimeBuilder`]. It preserves
//! the exact `submit → AgentEvent stream` contract those runners consume, so
//! the multi-step orchestration, output formatting, JSONL persistence, caching
//! and artifact logic above it stay byte-for-byte identical.
//!
//! Per-step **mode** and **model** overrides (from workflow `## step`
//! `<!-- sven: mode=… model=… -->` comments and frontmatter `models`) are hon
//! oured by rebuilding a fresh kernel [`SessionBundle`] for every turn, seeded
//! with the accumulated conversation history — the exact single-shot pattern of
//! [`RuntimeRunner`](crate::RuntimeRunner) applied per step. This keeps the
//! per-step override semantics of the legacy runner without needing a live
//! model/mode swap on a running kernel.

use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::mpsc;

use sven_bootstrap::{build_tool_registry, RuntimeBuilder, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_machines::AgentEvent;
use sven_hsm::{Event, UiEvent};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::ToolRegistry;

/// A kernel-backed replacement for the headless runners' use of
/// `sven_machines::Agent`.
///
/// Owns the reusable runtime context and accumulated conversation history, and
/// exposes the exact method slice ([`submit`](Self::submit),
/// [`seed_history`](Self::seed_history), [`set_mode`](Self::set_mode),
/// [`set_model_config`](Self::set_model_config),
/// [`build_tool_registry`](Self::build_tool_registry)) the runners drive.
pub struct KernelAgent {
    config: Arc<Config>,
    runtime_ctx: RuntimeContext,
    /// Current kernel mode string (`"agent"`, `"chat"`, `"sdlc"`, …) — selects
    /// which machine runs (`kernel_mode`: `Plan`/`Research`/`Agent` all drive
    /// the same "agent"-string reactive machine).
    mode: String,
    /// The original interactive [`AgentMode`], kept alongside the derived
    /// `mode` string and passed to `RuntimeBuilder::with_agent_mode` on every
    /// rebuilt session. `Plan`/`Research` collapse to the SAME kernel-mode
    /// string as `Agent` (see `kernel_mode`) since they drive the same
    /// machine; the read-only permission policy that's the entire point of
    /// those modes is selected by `with_agent_mode`, not by `mode`. Without
    /// this field, `--mode plan` silently ran with full write permissions in
    /// every headless run (`CiRunner`/`KernelAgent` is what `--output-trace`
    /// always routes through) -- confirmed against a real packaged build
    /// before this fix.
    agent_mode: AgentMode,
    /// Current model config for the next turn (per-step overridable).
    model_cfg: ModelConfig,
    /// Accumulated conversation history seeded into every rebuilt session.
    /// Mirrors the internal session history the legacy `Agent` maintained.
    history: Vec<Message>,
    /// MCP-tool wait budget (ms) applied to each rebuilt session.
    wait_for_mcp_ms: u64,
}

impl KernelAgent {
    /// Create a kernel agent for the given config, runtime context, initial
    /// mode, and model config.
    pub fn new(
        config: Arc<Config>,
        runtime_ctx: RuntimeContext,
        initial_mode: AgentMode,
        model_cfg: ModelConfig,
    ) -> Self {
        Self {
            config,
            runtime_ctx,
            mode: kernel_mode(initial_mode).to_string(),
            agent_mode: initial_mode,
            model_cfg,
            history: Vec::new(),
            wait_for_mcp_ms: 20_000,
        }
    }

    /// Override the mode used for subsequent turns (per-step `mode=` option).
    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = kernel_mode(mode).to_string();
        self.agent_mode = mode;
    }

    /// Override the model config used for subsequent turns (per-step
    /// `model=`/`provider=` options or frontmatter `models`).
    pub fn set_model_config(&mut self, cfg: ModelConfig) {
        self.model_cfg = cfg;
    }

    /// Seed prior conversation history (resumed JSONL / YAML chat / piped
    /// markdown). Replaces any existing accumulated history, mirroring the
    /// legacy `Agent::seed_history` used once before the step loop.
    pub fn seed_history(&mut self, messages: Vec<Message>) {
        self.history = messages;
    }

    /// Build a standalone [`ToolRegistry`] for tool-call replay
    /// (`--rerun-toolcalls`). The kernel builds its own registry internally per
    /// turn; replay only needs a functional registry to re-execute the recorded
    /// calls with fresh results, so a fresh Full-profile registry suffices.
    pub fn build_tool_registry(&self) -> anyhow::Result<Arc<ToolRegistry>> {
        let model =
            sven_model_drivers::from_config(&self.model_cfg).context("failed to initialise model provider")?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model);
        let mode_lock = Arc::new(tokio::sync::Mutex::new(AgentMode::Agent));
        let (tool_event_tx, _tool_event_rx) =
            mpsc::channel::<sven_tools::events::ToolEvent>(64);
        let todos = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(sven_tools::OutputBufferStore::new()));
        let profile = ToolSetProfile::Full {
            question_tx: None,
            todos,
            buffer_store,
        };
        let reg = build_tool_registry(
            &self.config,
            model,
            profile,
            mode_lock,
            tool_event_tx,
            self.runtime_ctx.to_agent_runtime(),
        );
        Ok(Arc::new(reg))
    }

    /// Run one turn on a freshly-built kernel session seeded with the
    /// accumulated history, streaming each mapped [`AgentEvent`] into `tx`.
    ///
    /// The user message and every resulting assistant/tool message are appended
    /// to the internal history so the next turn sees the full context — exactly
    /// as the legacy `Agent` maintained its session across `submit` calls.
    pub async fn submit(
        &mut self,
        text: &str,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let ctx = self.runtime_ctx.clone();
        let bundle = RuntimeBuilder::new(self.config.clone(), self.mode.clone())
            .with_runtime_context(ctx)
            .with_agent_mode(self.agent_mode)
            .with_model_config(self.model_cfg.clone())
            .with_allow_interactive_oauth(false)
            .with_wait_for_mcp_tools(self.wait_for_mcp_ms)
            .with_initial_history(self.history.clone())
            .build_session()
            .await
            .context("failed to build kernel session")?;

        // Auto-approve all human gates (headless CI is non-interactive).
        tokio::spawn(bundle.channels.auto_approve());

        let sink = bundle.handle.sink();
        let mut obs_rx = bundle.handle.subscribe_observations();

        // Record the user turn in the accumulated history before posting.
        self.history.push(Message::user(text));

        if !sink
            .emit(Event::UserMessage {
                text: text.to_string(),
            })
            .await
        {
            anyhow::bail!("kernel event queue closed before UserMessage was delivered");
        }

        loop {
            match obs_rx.recv().await {
                Ok(ev) => {
                    let terminal = matches!(ev, UiEvent::TurnComplete | UiEvent::Aborted { .. });
                    // Grow the internal history so the next turn is seeded
                    // with this turn's output (mirrors the legacy session).
                    reduce_history(&ev, &mut self.history);
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                    if terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }

        // Keep the runtime alive until the turn is fully drained so the audit
        // log flushes; dropping it shuts the session down.
        drop(bundle.runtime);
        Ok(())
    }
}

/// Map a caller [`AgentMode`] to a registered kernel mode. Coding-family modes
/// (agent/plan/research) all resolve to the reactive `agent` machine.
fn kernel_mode(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Chat => "chat",
        AgentMode::Sdlc => "sdlc",
        AgentMode::Agent | AgentMode::Plan | AgentMode::Research => "agent",
    }
}

/// Append this turn's produced messages to the accumulated history so the next
/// rebuilt session is seeded with them. Mirrors the message shapes the runners'
/// `handle_event`/`collect_event_full` push into their own `collected` copy.
///
/// Every non-handled variant is named explicitly (rather than a trailing
/// `_ => {}`) so a future variant that plausibly belongs in history forces a
/// decision here instead of being silently skipped.
fn reduce_history(ev: &AgentEvent, history: &mut Vec<Message>) {
    match ev {
        AgentEvent::TextComplete(text) if !text.is_empty() => {
            history.push(Message::assistant(text));
        }
        AgentEvent::ToolCallStarted(tc) => {
            history.push(Message {
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
        AgentEvent::ToolCallFinished { call_id, output, .. } => {
            history.push(Message::tool_result(call_id, output));
        }
        AgentEvent::Aborted { partial_text } if !partial_text.is_empty() => {
            history.push(Message::assistant(partial_text));
        }
        // Empty TextComplete/Aborted (already excluded above by the guards),
        // plus every event with no message-history representation: streaming
        // deltas (folded into the eventual TextComplete), progress/usage/
        // compaction telemetry, mode/model/todo bookkeeping, questions,
        // titles, team/subagent/peer observations, and the transition trace.
        AgentEvent::TextComplete(_)
        | AgentEvent::Aborted { .. }
        | AgentEvent::TextDelta(_)
        | AgentEvent::ThinkingDelta(_)
        | AgentEvent::ThinkingComplete(_)
        | AgentEvent::ToolProgress { .. }
        | AgentEvent::ContextCompacted { .. }
        | AgentEvent::TokenUsage { .. }
        | AgentEvent::TurnComplete
        | AgentEvent::Error(_)
        | AgentEvent::TodoUpdate(_)
        | AgentEvent::ModeChanged(_)
        | AgentEvent::ModelChanged(_)
        | AgentEvent::Question { .. }
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::TitleGenerated(_)
        | AgentEvent::CollabEvent(_)
        | AgentEvent::DelegateSummary { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentEvent { .. }
        | AgentEvent::PeerList(_)
        | AgentEvent::Transition { .. } => {}
    }
}
