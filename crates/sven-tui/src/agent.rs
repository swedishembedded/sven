// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Background agent task and request/event channel types.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use sven_bootstrap::{AgentBuilder, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_model::Message;
use sven_runtime::{SharedAgents, SharedSkills};
use sven_tools::{QuestionRequest, TodoItem};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

/// Request sent from the TUI to the background agent task.
///
/// All model overrides carry an already-resolved `ModelConfig`.  The TUI
/// resolves the config via `SessionState` and `sven_model::resolve_model_from_config`;
/// the agent task only calls `sven_model::from_config` to instantiate the
/// provider, never re-derives which model to use.
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
}

/// Background task that owns the `Agent` and forwards events back to the TUI.
///
/// The startup model is passed as an already-resolved `ModelConfig` (the TUI
/// applied the CLI `--model` override before spawning).  Per-message model
/// overrides in `AgentRequest` variants are also pre-resolved `ModelConfig`
/// values; this task only calls `from_config` to instantiate the provider.
///
/// `cancel_handle` is a shared slot that holds the sender half of a
/// per-submission `oneshot` channel.  The TUI drops (or sends on) the sender
/// to interrupt the current run.  The task creates a fresh channel before
/// every Submit/Resubmit and stores the sender in the slot; it is cleared
/// when the submission completes.
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
    let task_depth = Arc::new(AtomicUsize::new(0));
    let profile = ToolSetProfile::Full {
        question_tx: Some(question_tx),
        todos,
        task_depth,
    };

    // Build a RuntimeContext that uses the caller-provided SharedSkills and
    // SharedAgents so that a TUI `/refresh` updates both on the next turn.
    let runtime_ctx = {
        let mut ctx = RuntimeContext::auto_detect();
        ctx.skills = shared_skills;
        ctx.agents = shared_agents;
        ctx
    };

    let mut agent = AgentBuilder::new(config.clone())
        .with_runtime_context(runtime_ctx)
        .build(mode, model.clone(), profile)
        .await;

    // Model/mode overrides are applied permanently: no revert after the turn.
    let _ = model;
    let _ = mode;

    while let Some(req) = rx.recv().await {
        match req {
            AgentRequest::Submit {
                content,
                model_override,
                mode_override,
            } => {
                debug!(msg_len = content.len(), "agent task received message");

                if let Some(ref model_cfg) = model_override {
                    match sven_model::from_config(model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m) as Arc<dyn sven_model::ModelProvider>);
                        }
                        Err(e) => {
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
                    match sven_model::from_config(model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m) as Arc<dyn sven_model::ModelProvider>);
                        }
                        Err(e) => {
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
        }
    }
}
