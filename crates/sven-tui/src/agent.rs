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
    Submit(String),
    /// Replace conversation history and submit (edit-and-resubmit flow).
    Resubmit {
        messages: Vec<Message>,
        new_user_content: String,
    },
    /// Pre-load conversation history (resume flow). Does not trigger a model
    /// call; the agent is just primed for the next submission.
    LoadHistory(Vec<Message>),
}

/// Background task that owns the `Agent` and forwards events back to the TUI.
pub async fn agent_task(
    config: Arc<Config>,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
) {
    let model = match sven_model::from_config(&config.model) {
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

    let mut agent = AgentBuilder::new(config)
        .with_runtime_context(RuntimeContext::auto_detect())
        .build(mode, model, profile);

    while let Some(req) = rx.recv().await {
        match req {
            AgentRequest::Submit(msg) => {
                debug!(msg_len = msg.len(), "agent task received message");
                if let Err(e) = agent.submit(&msg, tx.clone()).await {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
            AgentRequest::Resubmit { messages, new_user_content } => {
                debug!("agent task received resubmit");
                if let Err(e) = agent
                    .replace_history_and_submit(messages, &new_user_content, tx.clone())
                    .await
                {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
            AgentRequest::LoadHistory(messages) => {
                debug!(n = messages.len(), "agent task loading history");
                agent.session_mut().replace_messages(messages);
            }
        }
    }
}
