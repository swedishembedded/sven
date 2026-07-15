// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background agent task and request/event channel types.
//!
//! This module is shared by all Sven frontends (TUI and GUI). It provides the
//! `AgentRequest` enum, the legacy `agent_task` background task (kept for
//! backward compatibility), and the new `kernel_session_task` that drives a
//! full [`RuntimeBuilder`] kernel session and bridges [`UiEvent`]s back to
//! the existing [`AgentEvent`] renderers.

use std::sync::Arc;

use futures::StreamExt;
use sven_bootstrap::{AgentBuilder, McpManager, RuntimeBuilder, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_hsm::UiEvent;
use sven_input::make_title;
use sven_mcp_client::McpEvent;
use sven_model::{CompletionRequest, Message, ResponseEvent};
use sven_runtime::{SharedAgents, SharedSkills};
use sven_tools::events::TodoItem;
use sven_tools::{OutputBufferStore, Question, QuestionRequest, SharedToolDisplays, SharedTools};
use sven_tools::Tool;
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

/// Background task that owns the `Agent` and forwards events back to the frontend.
///
/// The startup model is passed as an already-resolved `ModelConfig` (the
/// frontend applied the CLI `--model` override before spawning). Per-message
/// model overrides in `AgentRequest` variants are also pre-resolved
/// `ModelConfig` values; this task only calls `from_config` to instantiate
/// the provider.
///
/// `cancel_handle` is a shared slot that holds the sender half of a
/// per-submission `oneshot` channel. The frontend drops (or sends on) the
/// sender to interrupt the current run. The task creates a fresh channel
/// before every Submit/Resubmit and stores the sender in the slot; it is
/// cleared when the submission completes.
#[allow(clippy::too_many_arguments)]
pub async fn agent_task(
    config: Arc<Config>,
    startup_model_cfg: ModelConfig,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    cancel_handle: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    shared_skills: SharedSkills,
    shared_agents: SharedAgents,
    shared_tools: SharedTools,
    shared_tool_displays: SharedToolDisplays,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    mcp_manager_tx: Option<oneshot::Sender<(Arc<McpManager>, mpsc::Receiver<McpEvent>)>>,
    mcp_refresh_rx: Option<broadcast::Receiver<()>>,
) {
    let model: Arc<dyn sven_model::ModelProvider> =
        match sven_model::from_config(&startup_model_cfg) {
            Ok(m) => Arc::from(m),
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(format!("model init: {e}"))).await;
                return;
            }
        };

    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
    let profile = ToolSetProfile::Full {
        question_tx: Some(question_tx),
        todos,
        buffer_store: Arc::clone(&buffer_store),
    };

    let runtime_ctx = {
        let mut ctx = RuntimeContext::auto_detect();
        ctx.skills = shared_skills;
        ctx.agents = shared_agents;
        ctx
    };

    let shared_tools_loop = shared_tools.clone();
    let (mut agent, mcp_manager, mcp_event_rx) = AgentBuilder::new(config.clone())
        .with_runtime_context(runtime_ctx)
        .with_shared_tools(shared_tools)
        .with_shared_tool_displays(shared_tool_displays)
        .build_with_mcp(mode, model.clone(), profile)
        .await;

    if let Some(tx_mcp) = mcp_manager_tx {
        let _ = tx_mcp.send((Arc::clone(&mcp_manager), mcp_event_rx));
    }

    let _ = mode;

    let mut current_model_cfg = startup_model_cfg;

    let mut mcp_refresh_rx = mcp_refresh_rx;

    loop {
        let req = tokio::select! {
            biased;

            req = rx.recv() => match req {
                Some(r) => r,
                None => break,
            },

            result = async {
                if let Some(ref mut r) = mcp_refresh_rx {
                    r.recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                match result {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        let tools = mcp_manager.tools().await;
                        let tools: Vec<Arc<dyn Tool>> = tools
                            .into_iter()
                            .map(|t| Arc::new(t) as Arc<dyn Tool>)
                            .collect();
                        agent.refresh_mcp_tools(tools);
                        shared_tools_loop.set(agent.tools().schemas());
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
                continue;
            }
        };

        match req {
            AgentRequest::Submit {
                content,
                model_override,
                mode_override,
            } => {
                debug!(msg_len = content.len(), "agent task received message");

                if let Some(ref model_cfg) = model_override {
                    current_model_cfg = model_cfg.clone();
                    match sven_model::from_config(model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m) as Arc<dyn sven_model::ModelProvider>);
                        }
                        Err(e) => {
                            debug!(error = %e, "model override init failed, sending error to frontend");
                            let _ = tx
                                .send(AgentEvent::Error(format!("model override init: {e}")))
                                .await;
                            continue;
                        }
                    }
                }

                if let Some(m) = mode_override {
                    agent.set_mode(m).await;
                }

                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                *cancel_handle.lock().await = Some(cancel_tx);
                let result = agent
                    .submit_with_cancel(&content, tx.clone(), cancel_rx)
                    .await;
                cancel_handle.lock().await.take();
                if let Err(e) = result {
                    let _ = tx.send(AgentEvent::Error(format!("{:#}", e))).await;
                }
            }
            AgentRequest::Resubmit {
                messages,
                new_user_content,
                model_override,
                mode_override,
            } => {
                debug!("agent task received resubmit");

                if let Some(ref model_cfg) = model_override {
                    current_model_cfg = model_cfg.clone();
                    match sven_model::from_config(model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m) as Arc<dyn sven_model::ModelProvider>);
                        }
                        Err(e) => {
                            debug!(error = %e, "model override init failed on resubmit");
                            let _ = tx
                                .send(AgentEvent::Error(format!("model override init: {e}")))
                                .await;
                            continue;
                        }
                    }
                }

                if let Some(m) = mode_override {
                    agent.set_mode(m).await;
                }

                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                *cancel_handle.lock().await = Some(cancel_tx);
                let result = agent
                    .replace_history_and_submit_with_cancel(
                        messages,
                        &new_user_content,
                        tx.clone(),
                        cancel_rx,
                    )
                    .await;
                cancel_handle.lock().await.take();
                if let Err(e) = result {
                    let _ = tx.send(AgentEvent::Error(format!("{:#}", e))).await;
                }
            }
            AgentRequest::LoadHistory(messages) => {
                debug!(n = messages.len(), "agent task loading history");
                agent.seed_history(messages).await;
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
            AgentRequest::ListPeers => {
                // Only relevant in node-proxy mode; local agent ignores.
            }
            AgentRequest::RefreshMcpTools => {
                let tools = mcp_manager.tools().await;
                let tools: Vec<Arc<dyn Tool>> = tools
                    .into_iter()
                    .map(|t| Arc::new(t) as Arc<dyn Tool>)
                    .collect();
                agent.refresh_mcp_tools(tools);
                shared_tools_loop.set(agent.tools().schemas());
            }
        }
    }
}

// ── Kernel session task ───────────────────────────────────────────────────────

/// Kernel-backed replacement for [`agent_task`].
///
/// Builds a full [`RuntimeBuilder`] session (reactive `agent` mode), subscribes
/// to the outward observation bus, and bridges [`UiEvent`]s back to the
/// existing [`AgentEvent`] renderers so the TUI/GUI requires minimal changes.
///
/// The function signature intentionally mirrors [`agent_task`] to allow a
/// drop-in replacement in the callers.
#[allow(clippy::too_many_arguments)]
pub async fn kernel_session_task(
    config: Arc<Config>,
    startup_model_cfg: ModelConfig,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    cancel_handle: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
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

    // Unblock the TUI's mcp_rx.await by sending the manager immediately.
    // Without this the render loop never starts (blank screen).
    if let Some(mcp_tx) = mcp_manager_tx {
        let _ = mcp_tx.send((bundle.mcp_manager, bundle.mcp_event_rx));
    }

    let handle = bundle.handle.clone();
    let mut obs_rx = bundle.handle.subscribe_observations();

    // Drive the kernel runtime in the background (keeps it alive).
    let _runtime = bundle.runtime;

    // Bridge kernel-level AskUser / RequestHumanApproval to the TUI modals.
    // - UserQuestion → QuestionRequest on the tool question channel (the same
    //   channel the TUI's run loop selects on for the ask_question tool).
    // - ApprovalRequest → forwarded to the TUI as a QuestionRequest with
    //   yes/no options (ConfirmModal routing). The user must explicitly approve
    //   or deny each capability request; no blanket auto-approve in interactive
    //   sessions.
    let mut channels = bundle.channels;
    let bridge_question_tx = question_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                q = channels.question_rx.recv() => match q {
                    Some(kernel_q) => {
                        // Bridge the kernel clarification prompt to the TUI's
                        // QuestionModal via the tool question channel.
                        // Empty options → modal shows only the free-text "Other" row.
                        let (answer_tx, answer_rx) = oneshot::channel::<String>();
                        let req = QuestionRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            questions: vec![Question {
                                prompt: kernel_q.prompt.clone(),
                                options: vec![],
                                allow_multiple: false,
                            }],
                            answer_tx,
                        };
                        if bridge_question_tx.send(req).await.is_ok() {
                            // Wait for the user's answer and relay it back.
                            if let Ok(answer) = answer_rx.await {
                                let _ = kernel_q.reply_tx.send(answer);
                            }
                        } else {
                            // Channel closed (TUI exited) - fall back to empty reply.
                            let _ = kernel_q.reply_tx.send(String::new());
                        }
                    }
                    None => break,
                },
                a = channels.approval_rx.recv() => match a {
                    Some(a) => {
                        // Forward the approval request to the TUI as a ConfirmModal
                        // question (yes = allow, no = deny). The user must explicitly
                        // approve or deny in interactive sessions.
                        let prompt = format!(
                            "Allow {:?} capability?\n\nAction: {}",
                            a.capability, a.description
                        );
                        let (answer_tx, answer_rx) = oneshot::channel::<String>();
                        let req = QuestionRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            questions: vec![Question {
                                prompt,
                                options: vec!["yes".to_string(), "no".to_string()],
                                allow_multiple: false,
                            }],
                            answer_tx,
                        };
                        let approved = if bridge_question_tx.send(req).await.is_ok() {
                            answer_rx.await.map(|r| r.trim().to_lowercase() == "yes").unwrap_or(false)
                        } else {
                            false
                        };
                        let _ = a.reply_tx.send(approved);
                    }
                    None => break,
                },
            }
        }
    });

    // Spawn the observation bridge: UiEvent → AgentEvent.
    let event_tx_bridge = tx.clone();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match obs_rx.recv().await {
                Ok(ev) => {
                    let is_turn_complete = matches!(ev, sven_hsm::UiEvent::TurnComplete);
                    let is_text_complete = matches!(ev, sven_hsm::UiEvent::TextComplete(_));
                    let is_text_delta = matches!(ev, sven_hsm::UiEvent::TextDelta(_));
                    if is_turn_complete || is_text_complete || is_text_delta {
                        tracing::info!(
                            turn_complete = is_turn_complete,
                            text_complete = is_text_complete,
                            text_delta = is_text_delta,
                            "bridge: received UiEvent from kernel"
                        );
                    }
                    if let Some(ae) = ui_event_to_agent_event(ev) {
                        if let Err(e) = event_tx_bridge.send(ae).await {
                            tracing::warn!("bridge: failed to send AgentEvent to TUI: {e}");
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "bridge: lagged, skipped events");
                    continue;
                }
                Err(RecvError::Closed) => {
                    tracing::info!("bridge: observation channel closed, bridge exiting");
                    break;
                }
            }
        }
    });

    let current_model_cfg = Arc::new(tokio::sync::Mutex::new(startup_model_cfg));

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
                if let Some(ref model_cfg) = model_override {
                    *current_model_cfg.lock().await = model_cfg.clone();
                }
                let _ = mode_override;
                debug!(
                    msg_len = content.len(),
                    "kernel task: posting UserMessage (Submit)"
                );
                tracing::info!(
                    msg_len = content.len(),
                    "kernel_session_task: sending UserMessage to HSM (Submit)"
                );
                if !handle.send_user_message(content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
                tracing::info!("kernel_session_task: UserMessage queued");
            }

            AgentRequest::Resubmit {
                messages,
                new_user_content,
                model_override,
                mode_override,
            } => {
                debug!("kernel task: resubmit");
                tracing::info!(
                    history_len = messages.len(),
                    "kernel_session_task: Resubmit received"
                );
                if let Some(ref model_cfg) = model_override {
                    *current_model_cfg.lock().await = model_cfg.clone();
                }
                let _ = mode_override;
                // History seeding for resubmit is handled by the kernel's ConversationStore.
                let _ = messages;
                tracing::info!("kernel_session_task: sending UserMessage to HSM (Resubmit)");
                if !handle.send_user_message(new_user_content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
                tracing::info!("kernel_session_task: UserMessage queued");
            }

            AgentRequest::LoadHistory(messages) => {
                debug!(
                    n = messages.len(),
                    "kernel task: load history (no-op in kernel mode)"
                );
                let _ = messages;
            }

            AgentRequest::GenerateTitle { user_text } => {
                let cfg = current_model_cfg.lock().await.clone();
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

/// Bridge a [`UiEvent`] from the outward observation plane back to the
/// corresponding [`AgentEvent`] expected by existing TUI/GUI renderers.
///
/// Returns `None` for observation-only events that have no `AgentEvent`
/// equivalent (e.g. transition traces).
fn ui_event_to_agent_event(ev: UiEvent) -> Option<AgentEvent> {
    // Single source of truth lives in `sven-bootstrap` so every surface
    // (CI/node/ACP/frontend) maps the kernel's observation plane identically.
    sven_bootstrap::ui_event_to_agent_event(ev)
}
