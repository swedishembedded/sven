// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background agent task and request/event channel types.
//!
//! This module is shared by all Sven frontends (TUI and GUI). It provides the
//! `AgentRequest` enum and the `agent_task` background task that owns the
//! `Agent` and forwards events back to the frontend.

use std::sync::Arc;

use futures::StreamExt;
use sven_bootstrap::{AgentBuilder, McpManager, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_input::make_title;
use sven_mcp_client::McpEvent;
use sven_model::{CompletionRequest, Message, ResponseEvent};
use sven_runtime::{SharedAgents, SharedSkills};
use sven_tools::Tool;
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays, SharedTools, TodoItem};
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
