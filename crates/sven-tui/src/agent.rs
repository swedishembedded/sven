//! Background agent task and request/event channel types.

use std::sync::Arc;

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent};
use sven_model::Message;
use sven_tools::{
    AskQuestionTool, FsTool, GlobTool, QuestionRequest, ShellTool, ToolRegistry,
    GdbStartServerTool, GdbConnectTool, GdbCommandTool, GdbInterruptTool, GdbStopTool,
    GdbSessionState,
};
use tokio::sync::mpsc;
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

    let mut registry = ToolRegistry::new();
    registry.register(ShellTool { timeout_secs: config.tools.timeout_secs });
    registry.register(FsTool);
    registry.register(GlobTool);
    registry.register(AskQuestionTool::new_tui(question_tx));

    let gdb_state = Arc::new(tokio::sync::Mutex::new(GdbSessionState::default()));
    registry.register(GdbStartServerTool::new(gdb_state.clone(), config.tools.gdb.clone()));
    registry.register(GdbConnectTool::new(gdb_state.clone(), config.tools.gdb.clone()));
    registry.register(GdbCommandTool::new(gdb_state.clone()));
    registry.register(GdbInterruptTool::new(gdb_state.clone()));
    registry.register(GdbStopTool::new(gdb_state));

    let mut agent = Agent::new(
        model,
        Arc::new(registry),
        Arc::new(config.agent.clone()),
        mode,
        128_000,
    );

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
