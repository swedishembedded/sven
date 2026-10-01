// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The small agent API the headless runner ([`CiRunner`](crate::CiRunner))
//! depends on, backed by the HSM kernel.
//!
//! [`KernelAgent`] runs each turn on an HSM kernel session opened through the
//! frontend's [`SessionController`] and exposes a `submit → AgentEvent stream` contract to
//! the multi-step orchestration, output formatting, JSONL persistence,
//! caching and artifact logic above it.
//!
//! Per-step **mode** and **model** overrides (from workflow `## step`
//! `<!-- sven: mode=… model=… -->` comments and frontmatter `models`) are hon
//! oured by rebuilding a fresh kernel [`SessionBundle`] for every turn, seeded
//! with the accumulated conversation history — the exact single-shot pattern of
//! [`RuntimeRunner`](crate::RuntimeRunner) applied per step. This gives
//! per-step overrides without needing a live model/mode swap on a running
//! kernel.

use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::mpsc;

use sven_bootstrap::Config;
use sven_bootstrap::{build_tool_registry, RuntimeContext, ToolSetProfile};
use sven_frontend::{SessionController, SessionOptions, SessionSpec};
use sven_machines::AgentEvent;
use sven_model::Message;
use sven_model_drivers::ModelConfig;
use sven_session_model::reduce_history;
use sven_tool_registry::ToolRegistry;
use sven_vocab::AgentMode;

/// The kernel-backed agent the headless runner drives.
///
/// Owns the reusable runtime context and accumulated conversation history, and
/// exposes the exact method slice ([`submit`](Self::submit),
/// [`seed_history`](Self::seed_history), [`set_mode`](Self::set_mode),
/// [`set_model_config`](Self::set_model_config),
/// [`build_tool_registry`](Self::build_tool_registry)) the runners drive.
pub struct KernelAgent {
    config: Arc<Config>,
    runtime_ctx: RuntimeContext,
    /// The interactive [`AgentMode`] each turn's session runs as. It selects
    /// the machine (`Plan`/`Research`/`Agent` all drive the same reactive
    /// one) and, what is the entire point of `Plan` and `Research`, the
    /// permissions: a read-only mode forbids writing at the kernel gate.
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
            agent_mode: initial_mode,
            model_cfg,
            history: Vec::new(),
            wait_for_mcp_ms: 20_000,
        }
    }

    /// Override the mode used for subsequent turns (per-step `mode=` option).
    pub fn set_mode(&mut self, mode: AgentMode) {
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
        let model = sven_model_drivers::from_config(&self.model_cfg)
            .context("failed to initialise model provider")?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model);
        let mode_lock = Arc::new(tokio::sync::Mutex::new(AgentMode::Agent));
        let (tool_event_tx, _tool_event_rx) = mpsc::channel::<sven_tool_api::events::ToolEvent>(64);
        let todos = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(
            sven_tools_fs::OutputBufferStore::new(),
        ));
        let profile = ToolSetProfile::Full {
            questions: sven_bootstrap::Questions::Unavailable,
            todos,
            buffer_store,
        };
        let config = Config {
            model: self.model_cfg.clone(),
            ..(*self.config).clone()
        };
        let reg = build_tool_registry(
            &config,
            model,
            profile,
            mode_lock,
            tool_event_tx,
            self.runtime_ctx.to_agent_runtime(),
        );
        Ok(Arc::new(reg))
    }

    /// Opens a fresh kernel session for the next turn, seeded with `history`.
    /// Nobody is at a headless run, so its gates are answered at once, and an
    /// MCP server that wants a browser sign-in is not given one.
    async fn open_session(
        &self,
        history: Vec<Message>,
    ) -> anyhow::Result<(SessionController, mpsc::Receiver<AgentEvent>)> {
        let (events, events_rx) = mpsc::channel(EVENT_CAPACITY);
        let mut options =
            SessionOptions::new(Arc::clone(&self.config), self.runtime_ctx.clone(), events);
        options.allow_interactive_oauth = false;
        options.wait_for_mcp_tools_ms = Some(self.wait_for_mcp_ms);
        let spec = SessionSpec::for_mode(self.agent_mode, self.model_cfg.clone());
        let (session, _mcp_events) = SessionController::open(options, spec, history)
            .await
            .context("failed to build kernel session")?;
        Ok((session, events_rx))
    }

    /// Run one turn on a freshly-built kernel session seeded with the
    /// accumulated history, streaming each mapped [`AgentEvent`] into `tx`.
    ///
    /// The user message and every resulting assistant/tool message are appended
    /// to the internal history so the next turn sees the full context — exactly
    /// as the legacy `Agent` maintained its session across `submit` calls.
    pub async fn submit(&mut self, text: &str, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let (session, mut events_rx) = self.open_session(self.history.clone()).await?;

        // Record the user turn in the accumulated history before posting.
        self.history.push(Message::user(text));

        session
            .post(text.to_string())
            .await
            .map_err(|e| anyhow::anyhow!("{e} before UserMessage was delivered"))?;

        Self::drain_turn(&mut events_rx, &mut self.history, &tx).await;

        // Keep the runtime alive until the turn is fully drained so the audit
        // log flushes; dropping it shuts the session down.
        session.detach();
        Ok(())
    }

    /// Multimodal variant of [`Self::submit`] for `--attach`-preloaded
    /// content parts (image/audio) that can't be expressed as plain text.
    ///
    /// The kernel event vocabulary (`sven_hsm::Event::UserMessage`) only
    /// carries a text payload — there is no parts-carrying variant, and
    /// adding one is a cross-cutting change (new `Event` variant, every
    /// machine/executor that reads it) out of scope for a single attach
    /// flow. Instead this seeds the parts message directly into the fresh
    /// session's conversation thread via the initial history (which
    /// accepts full `Message`s, parts included) and then posts a user message
    /// with an *empty* text: `TurnExecutor` only
    /// appends its `instruction` to the thread `if !req.instruction.is_empty()`
    /// (`crates/executors/src/turn.rs`), so the empty text drives the turn
    /// without appending a second, duplicate plain-text message.
    pub async fn submit_with_parts(
        &mut self,
        parts: Vec<sven_model::ContentPart>,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let user_msg = Message::user_with_parts(parts);
        let mut seeded_history = self.history.clone();
        seeded_history.push(user_msg.clone());

        let (session, mut events_rx) = self.open_session(seeded_history).await?;

        // Record the user turn in the accumulated history before posting.
        self.history.push(user_msg);

        session
            .post(String::new())
            .await
            .map_err(|e| anyhow::anyhow!("{e} before UserMessage was delivered"))?;

        Self::drain_turn(&mut events_rx, &mut self.history, &tx).await;

        // Keep the runtime alive until the turn is fully drained so the audit
        // log flushes; dropping it shuts the session down.
        session.detach();
        Ok(())
    }

    /// Build an [`sven_model::ModelProvider`] for the agent's *current*
    /// model config, for callers that need to inspect its modality support
    /// (`--attach`'s native-vs-transcribed decision) before submitting a
    /// turn. Mirrors the construction [`Self::build_tool_registry`] already
    /// does for the same config.
    pub fn model(&self) -> anyhow::Result<Arc<dyn sven_model::ModelProvider>> {
        let model = sven_model_drivers::from_config(&self.model_cfg)
            .context("failed to initialise model provider")?;
        Ok(Arc::from(model))
    }

    /// Drain one turn's event stream into `tx`, growing `history` via
    /// [`reduce_history`] as events arrive, until a terminal event
    /// ([`AgentEvent::TurnComplete`]/[`AgentEvent::Aborted`]) or the stream
    /// closes. Shared by [`Self::submit`] and [`Self::submit_with_parts`].
    async fn drain_turn(
        events_rx: &mut mpsc::Receiver<AgentEvent>,
        history: &mut Vec<Message>,
        tx: &mpsc::Sender<AgentEvent>,
    ) {
        while let Some(ev) = events_rx.recv().await {
            let terminal = matches!(ev, AgentEvent::TurnComplete | AgentEvent::Aborted { .. });
            // Grow the internal history so the next turn is seeded
            // with this turn's output (mirrors the legacy session).
            reduce_history(&ev, history);
            if tx.send(ev).await.is_err() || terminal {
                break;
            }
        }
    }
}

/// Capacity of the channel a turn's events travel on.
const EVENT_CAPACITY: usize = 256;
