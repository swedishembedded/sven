// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background agent task and request/event channel types.
//!
//! This module is shared by all Sven frontends (TUI and GUI). It provides the
//! `AgentRequest` enum and the `kernel_session_task` that drives a full
//! [`RuntimeBuilder`] kernel session (via the shared [`KernelAgentSession`]
//! adapter) and bridges [`UiEvent`](sven_hsm::UiEvent)s back to the existing
//! [`AgentEvent`] renderers so the TUI/GUI require no changes.

use std::sync::Arc;

use futures::StreamExt;
use sven_bootstrap::{KernelAgentSession, McpManager, RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_input::make_title;
use sven_mcp_client::McpEvent;
use sven_model::{CompletionRequest, Message, ResponseEvent};
use sven_runtime::{SharedAgents, SharedSkills};
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays, SharedTools};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tracing::{debug, warn};

/// Request sent from a frontend to the background agent task.
///
/// All model overrides carry an already-resolved `ModelConfig`. The frontend
/// resolves the config via `sven_model::resolve_model_from_config`; the agent
/// task only calls `sven_model::from_config` to instantiate the provider,
/// never re-derives which model to use.
#[derive(Debug)]
pub enum AgentRequest {
    /// Submit a new user message (normal flow).
    Submit {
        content: String,
        /// Pre-resolved model config; agent calls `from_config` to instantiate.
        model_override: Option<ModelConfig>,
        mode_override: Option<AgentMode>,
    },
    /// Replace conversation history and submit (edit-and-resubmit flow).
    Resubmit {
        messages: Vec<Message>,
        new_user_content: String,
        /// Pre-resolved model config; agent calls `from_config` to instantiate.
        model_override: Option<ModelConfig>,
        mode_override: Option<AgentMode>,
    },
    /// Pre-load conversation history (resume flow). Does not trigger a model
    /// call; the agent is just primed for the next submission.
    LoadHistory(Vec<Message>),
    /// Generate a short chat title from the first user message (LLM, low
    /// max_tokens, no tools). Result is sent as `AgentEvent::TitleGenerated`.
    GenerateTitle { user_text: String },
    /// Request peer list (node-proxy mode); handled by the mux, not the agent task.
    ListPeers,
    /// Refresh MCP tools from the manager (e.g. when ToolsChanged fires).
    RefreshMcpTools,
}

/// Lightweight helper to generate a title from a given model configuration.
async fn generate_title_with_config(cfg: &ModelConfig, user_text: &str) -> Option<String> {
    const TITLE_MAX_TOKENS: u32 = 50;

    let title_model = match sven_model::from_config(cfg) {
        Ok(m) => m,
        Err(e) => {
            warn!(provider = %cfg.provider, model = %cfg.name, error = %e, "title model init failed");
            return None;
        }
    };

    let req = CompletionRequest {
        messages: vec![
            Message::system(
                "Generate a very short conversation title (3-5 words, no quotes). Reply with only the title.",
            ),
            Message::user(user_text.trim()),
        ],
        tools: vec![],
        stream: true,
        system_dynamic_suffix: None,
        cache_key: None,
        max_output_tokens_override: Some(TITLE_MAX_TOKENS),
        core_tool_count: 0,
        response_format: None,
    };

    match title_model.complete(req).await {
        Ok(mut stream) => {
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(ResponseEvent::TextDelta(d)) => text.push_str(&d),
                    Ok(ResponseEvent::Done | ResponseEvent::MaxTokens) => break,
                    Ok(ResponseEvent::ThinkingDelta(_)) => (),
                    Err(e) => {
                        warn!(error = %e, "title generation stream error");
                        return None;
                    }
                    _ => {}
                }
            }
            let t = text.trim().trim_matches('"').trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Err(e) => {
            warn!(error = %e, "title generation request failed");
            None
        }
    }
}

// ── Kernel session task ───────────────────────────────────────────────────────

/// Background task that owns an HSM-kernel session and forwards its
/// [`AgentEvent`] stream back to the frontend.
///
/// Builds a full [`RuntimeBuilder`] session for the requested mode, then wraps
/// it in the shared [`KernelAgentSession`] adapter, which subscribes to the
/// kernel's outward observation bus and bridges [`UiEvent`](sven_hsm::UiEvent)s
/// into the existing [`AgentEvent`] renderers plus relays kernel `AskUser` /
/// approval prompts to the frontend's [`QuestionRequest`] modal channel. The
/// TUI/GUI therefore keep consuming the exact same `AgentEvent` contract.
///
/// The startup model is passed as an already-resolved `ModelConfig` (the
/// frontend applied the CLI `--model` override before spawning). Per-message
/// model overrides in `AgentRequest` variants are also pre-resolved
/// `ModelConfig` values; this task tracks the current one so title generation
/// uses the active model.
///
/// `cancel_handle` is a shared slot holding the sender half of a per-submission
/// cancellation channel; it is wired into the kernel session at build time so
/// the frontend can interrupt the in-flight turn.
#[allow(clippy::too_many_arguments)]
pub async fn kernel_session_task(
    config: Arc<Config>,
    startup_model_cfg: ModelConfig,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    _shared_skills: SharedSkills,
    _shared_agents: SharedAgents,
    _shared_tools: SharedTools,
    _shared_tool_displays: SharedToolDisplays,
    _buffer_store: Arc<Mutex<OutputBufferStore>>,
    mcp_manager_tx: Option<oneshot::Sender<(Arc<McpManager>, mpsc::Receiver<McpEvent>)>>,
    _mcp_refresh_rx: Option<broadcast::Receiver<()>>,
) {
    let kernel_mode = mode_to_kernel_mode(mode);
    let ctx = RuntimeContext::auto_detect();
    let bundle = match RuntimeBuilder::new(config.clone(), kernel_mode)
        .with_runtime_context(ctx)
        .with_model_config(startup_model_cfg.clone())
        .with_tool_question_tx(question_tx.clone())
        .with_cancel_handle(cancel_handle)
        .build_session()
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let _ = tx
                .send(AgentEvent::Error(format!("kernel session init: {e:#}")))
                .await;
            return;
        }
    };

    // Wrap the session in the shared adapter: it subscribes to observations
    // (before returning, so no post-first-send event is missed), spawns the
    // UiEvent → AgentEvent observation bridge into `tx`, and spawns the
    // AskUser / approval bridge onto the frontend's `question_tx` modal channel.
    let (session, mcp_event_rx) = KernelAgentSession::spawn(bundle, tx.clone(), question_tx);

    // Unblock the TUI's `mcp_rx.await` by sending the manager immediately.
    // Without this the render loop never starts (blank screen).
    if let Some(mcp_tx) = mcp_manager_tx {
        let _ = mcp_tx.send((session.mcp_manager(), mcp_event_rx));
    }

    // Track the active model config so `GenerateTitle` uses the current model.
    let mut current_model_cfg = startup_model_cfg;

    loop {
        let req = match rx.recv().await {
            Some(r) => r,
            None => break,
        };

        match req {
            AgentRequest::Submit {
                content,
                model_override,
                mode_override,
            } => {
                if let Some(model_cfg) = model_override {
                    current_model_cfg = model_cfg;
                }
                let _ = mode_override;
                debug!(msg_len = content.len(), "kernel task: posting UserMessage (Submit)");
                if !session.send_user_message(content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
            }

            AgentRequest::Resubmit {
                messages,
                new_user_content,
                model_override,
                mode_override,
            } => {
                debug!("kernel task: resubmit");
                if let Some(model_cfg) = model_override {
                    current_model_cfg = model_cfg;
                }
                let _ = mode_override;
                // History seeding for resubmit is handled by the kernel's
                // ConversationStore; only the new user message is posted.
                let _ = messages;
                if !session.send_user_message(new_user_content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
            }

            AgentRequest::LoadHistory(messages) => {
                debug!(
                    n = messages.len(),
                    "kernel task: load history (no-op in kernel mode)"
                );
                let _ = messages;
            }

            AgentRequest::GenerateTitle { user_text } => {
                let cfg = current_model_cfg.clone();
                let event_tx = tx.clone();
                tokio::spawn(async move {
                    let openrouter_title = {
                        let mut free_cfg = cfg.clone();
                        free_cfg.provider = "openrouter".to_string();
                        free_cfg.name = "openrouter/free".to_string();
                        free_cfg.base_url = None;
                        generate_title_with_config(&free_cfg, &user_text).await
                    };
                    let title = if openrouter_title.is_some() {
                        openrouter_title
                    } else {
                        generate_title_with_config(&cfg, &user_text).await
                    };
                    let final_title = title.unwrap_or_else(|| make_title(&user_text));
                    let _ = event_tx.send(AgentEvent::TitleGenerated(final_title)).await;
                });
            }

            AgentRequest::ListPeers | AgentRequest::RefreshMcpTools => {
                // Not applicable in kernel mode (MCP is initialized at session
                // build time; peer discovery is a node-only concern).
            }
        }
    }
}

/// Select the kernel mode string for a given [`AgentMode`].
///
/// The `"agent"` / reactive machine handles all coding-oriented modes;
/// `"chat"` and `"sdlc"` map to their dedicated machines.
fn mode_to_kernel_mode(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Chat => "chat",
        AgentMode::Sdlc => "sdlc",
        _ => "agent",
    }
}
