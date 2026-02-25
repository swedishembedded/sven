// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Background agent task and request/event channel types.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use sven_bootstrap::{AgentBuilder, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config};
use sven_core::AgentEvent;
use sven_model::Message;
use sven_tools::{QuestionRequest, TodoItem};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

/// Request sent from the TUI to the background agent task.
#[derive(Debug)]
pub enum AgentRequest {
    /// Submit a new user message (normal flow).
    ///
    /// `model_override` and `mode_override` are per-message overrides that
    /// take effect only for this turn and do not change the agent's baseline.
    Submit {
        content: String,
        model_override: Option<String>,
        mode_override: Option<AgentMode>,
    },
    /// Replace conversation history and submit (edit-and-resubmit flow).
    Resubmit {
        messages: Vec<Message>,
        new_user_content: String,
        model_override: Option<String>,
        mode_override: Option<AgentMode>,
    },
    /// Pre-load conversation history (resume flow). Does not trigger a model
    /// call; the agent is just primed for the next submission.
    LoadHistory(Vec<Message>),
}

/// Background task that owns the `Agent` and forwards events back to the TUI.
///
/// When `model_override` is `Some`, the effective model configuration is
/// resolved using the same logic as the CI runner (including the
/// `config.providers` map for named custom providers).
pub async fn agent_task(
    config: Arc<Config>,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    model_override: Option<String>,
) {
    let model_cfg = if let Some(ref mo) = model_override {
        sven_model::resolve_model_from_config(&config, mo)
    } else {
        config.model.clone()
    };

    let model: Arc<dyn sven_model::ModelProvider> = match sven_model::from_config(&model_cfg) {
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

    let mut agent = AgentBuilder::new(config.clone())
        .with_runtime_context(RuntimeContext::auto_detect())
        .build(mode, model.clone(), profile);

    while let Some(req) = rx.recv().await {
        match req {
            AgentRequest::Submit { content, model_override: msg_model_override, mode_override: msg_mode_override } => {
                debug!(msg_len = content.len(), "agent task received message");

                // Apply per-message model override: swap model in, run, swap back.
                let model_switched = if let Some(ref mo) = msg_model_override {
                    let msg_model_cfg = sven_model::resolve_model_from_config(&config, mo);
                    match sven_model::from_config(&msg_model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m));
                            true
                        }
                        Err(e) => {
                            let _ = tx
                                .send(AgentEvent::Error(format!("model override init: {e}")))
                                .await;
                            continue;
                        }
                    }
                } else {
                    false
                };

                // Apply per-message mode override.
                let mode_switched = if let Some(m) = msg_mode_override {
                    agent.set_mode(m).await;
                    true
                } else {
                    false
                };

                let result = agent.submit(&content, tx.clone()).await;

                // Restore baseline model/mode after the turn.
                if model_switched { agent.set_model(model.clone()); }
                if mode_switched  { agent.set_mode(mode).await; }

                if let Err(e) = result {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
            AgentRequest::Resubmit { messages, new_user_content, model_override: msg_model_override, mode_override: msg_mode_override } => {
                debug!("agent task received resubmit");

                let model_switched = if let Some(ref mo) = msg_model_override {
                    let msg_model_cfg = sven_model::resolve_model_from_config(&config, mo);
                    match sven_model::from_config(&msg_model_cfg) {
                        Ok(m) => {
                            agent.set_model(Arc::from(m));
                            true
                        }
                        Err(e) => {
                            let _ = tx
                                .send(AgentEvent::Error(format!("model override init: {e}")))
                                .await;
                            continue;
                        }
                    }
                } else {
                    false
                };

                let mode_switched = if let Some(m) = msg_mode_override {
                    agent.set_mode(m).await;
                    true
                } else {
                    false
                };

                let result = agent
                    .replace_history_and_submit(messages, &new_user_content, tx.clone())
                    .await;

                if model_switched { agent.set_model(model.clone()); }
                if mode_switched  { agent.set_mode(mode).await; }

                if let Err(e) = result {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
            AgentRequest::LoadHistory(messages) => {
                debug!(n = messages.len(), "agent task loading history");
                // seed_history strips system messages and prepends a fresh one,
                // ensuring the agent always has the correct system prompt.
                agent.seed_history(messages).await;
            }
        }
    }
}
