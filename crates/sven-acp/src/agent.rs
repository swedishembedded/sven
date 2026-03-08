// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`SvenAcpAgent`] — implements the ACP `Agent` trait for sven.
//!
//! Each `new_session` call builds a fresh `sven_core::Agent` via
//! [`sven_bootstrap::AgentBuilder`] and stores it in a [`SessionEntry`] keyed
//! by ACP [`SessionId`].  `prompt` runs the agent loop, bridges
//! [`sven_core::AgentEvent`]s to ACP `session/update` notifications, and
//! returns when the turn completes or is cancelled.
//!
//! The struct is intentionally `!Send` (it uses `RefCell` for interior
//! mutability) and lives inside a `tokio::task::LocalSet` spawned by
//! [`crate::serve_stdio`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::{
    AgentCapabilities, AuthenticateRequest, AuthenticateResponse, CancelNotification, Error,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, Result as AcpResult, SessionMode, SessionModeId, SessionModeState,
    SessionNotification, SetSessionModeRequest, SetSessionModeResponse, StopReason,
};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, warn};

use sven_bootstrap::{AgentBuilder, RuntimeContext, ToolSetProfile};
use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent};
use sven_tools::events::TodoItem;

use crate::bridge::{
    acp_mode_id_to_sven_mode, agent_event_to_session_update, sven_mode_to_acp_mode_id,
};

// ─── Version string ───────────────────────────────────────────────────────────

const SVEN_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Inter-task messaging ─────────────────────────────────────────────────────

/// Messages sent from the `Agent` trait methods to the background task that
/// owns the [`AgentSideConnection`] so it can call `conn.session_notification`.
pub enum ConnMessage {
    SessionUpdate(SessionNotification, oneshot::Sender<()>),
}

// ─── Session entry ────────────────────────────────────────────────────────────

/// Per-session state stored inside [`SvenAcpAgent`].
struct SessionEntry {
    /// The sven core agent for this session.
    agent: Mutex<Agent>,
    /// Mode lock shared between the agent loop and mode-change requests.
    mode_lock: Arc<Mutex<AgentMode>>,
    /// Cancellation sender; replaced on each new prompt turn.
    cancel_tx: Mutex<Option<oneshot::Sender<()>>>,
}

// ─── SvenAcpAgent ─────────────────────────────────────────────────────────────

/// ACP agent implementation backed by a sven [`Agent`].
///
/// `!Send` due to `RefCell`; must run inside a [`tokio::task::LocalSet`].
pub struct SvenAcpAgent {
    config: Arc<Config>,
    sessions: RefCell<HashMap<String, Arc<SessionEntry>>>,
    conn_tx: mpsc::UnboundedSender<ConnMessage>,
}

impl SvenAcpAgent {
    pub fn new(config: Arc<Config>, conn_tx: mpsc::UnboundedSender<ConnMessage>) -> Self {
        Self {
            config,
            sessions: RefCell::new(HashMap::new()),
            conn_tx,
        }
    }

    /// Clone the session `Arc` out of the `RefCell` without holding a borrow
    /// across an `.await` point.
    fn get_session(&self, session_id: &str) -> Option<Arc<SessionEntry>> {
        self.sessions.borrow().get(session_id).cloned()
    }

    /// Send one `session/update` notification to the client via the background
    /// task.  Blocks until the notification has been dispatched.
    async fn send_notification(&self, notification: SessionNotification) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .conn_tx
            .send(ConnMessage::SessionUpdate(notification, ack_tx))
            .is_ok()
        {
            let _ = ack_rx.await;
        }
    }

    /// Build the list of advertised [`SessionMode`]s.
    fn advertised_modes() -> Vec<SessionMode> {
        vec![
            SessionMode::new(SessionModeId::new("agent"), "Agent").description(
                "Full agentic mode: reads, writes, executes tools autonomously".to_string(),
            ),
            SessionMode::new(SessionModeId::new("plan"), "Plan")
                .description("Planning mode: proposes changes without writing files".to_string()),
            SessionMode::new(SessionModeId::new("research"), "Research")
                .description("Research mode: reads and searches, no file writes".to_string()),
        ]
    }
}

// ─── ACP Agent trait implementation ──────────────────────────────────────────

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Agent for SvenAcpAgent {
    async fn initialize(&self, args: InitializeRequest) -> AcpResult<InitializeResponse> {
        debug!(
            "ACP initialize: protocol_version={:?}",
            args.protocol_version
        );
        Ok(InitializeResponse::new(args.protocol_version)
            .agent_capabilities(AgentCapabilities::new())
            .agent_info(
                agent_client_protocol::Implementation::new("sven", SVEN_VERSION)
                    .title("Sven AI Coding Agent".to_string()),
            ))
    }

    async fn authenticate(&self, _args: AuthenticateRequest) -> AcpResult<AuthenticateResponse> {
        Ok(AuthenticateResponse::new())
    }

    async fn new_session(&self, args: NewSessionRequest) -> AcpResult<NewSessionResponse> {
        debug!("ACP new_session: cwd={:?}", args.cwd);

        let session_id = uuid::Uuid::new_v4().to_string();
        let initial_mode = AgentMode::Agent;

        let model: Arc<dyn sven_model::ModelProvider> =
            match sven_model::from_config(&self.config.model) {
                Ok(m) => Arc::from(m),
                Err(e) => {
                    tracing::error!("ACP model init error: {e}");
                    return Err(Error::internal_error());
                }
            };

        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(vec![]));
        let profile = ToolSetProfile::Full {
            question_tx: None,
            todos: todos.clone(),
        };

        let mut runtime_ctx = RuntimeContext::auto_detect();
        runtime_ctx.project_root = Some(args.cwd.clone());

        let agent = AgentBuilder::new(Arc::clone(&self.config))
            .with_runtime_context(runtime_ctx)
            .build(initial_mode, model, profile)
            .await;

        let mode_lock = agent.current_mode_lock().clone();

        let entry = Arc::new(SessionEntry {
            agent: Mutex::new(agent),
            mode_lock,
            cancel_tx: Mutex::new(None),
        });

        self.sessions.borrow_mut().insert(session_id.clone(), entry);

        let mode_state = SessionModeState::new(
            sven_mode_to_acp_mode_id(initial_mode),
            Self::advertised_modes(),
        );

        Ok(NewSessionResponse::new(session_id).modes(mode_state))
    }

    async fn prompt(&self, args: PromptRequest) -> AcpResult<PromptResponse> {
        let session_id = args.session_id.to_string();
        debug!("ACP prompt: session={session_id}");

        let entry = self
            .get_session(&session_id)
            .ok_or_else(Error::invalid_params)?;

        // Extract text content from the prompt.
        let text = args
            .prompt
            .into_iter()
            .filter_map(|block| match block {
                agent_client_protocol::ContentBlock::Text(t) => Some(t.text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Set up cancellation.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *entry.cancel_tx.lock().await = Some(cancel_tx);

        // Event channel for streaming agent events.
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(128);

        // Spawn the agent task.
        let entry_for_task = Arc::clone(&entry);
        let text_clone = text.clone();
        let task = tokio::task::spawn_local(async move {
            let mut agent = entry_for_task.agent.lock().await;
            agent
                .submit_with_cancel(&text_clone, event_tx, cancel_rx)
                .await
        });

        // Bridge AgentEvents to ACP session/update notifications.
        let stop_reason = loop {
            match event_rx.recv().await {
                Some(AgentEvent::TurnComplete) => {
                    break StopReason::EndTurn;
                }
                Some(AgentEvent::Aborted { .. }) => {
                    break StopReason::Cancelled;
                }
                Some(event) => {
                    if let Some(update) = agent_event_to_session_update(&event) {
                        let notification =
                            SessionNotification::new(args.session_id.clone(), update);
                        self.send_notification(notification).await;
                    }
                }
                None => {
                    break StopReason::EndTurn;
                }
            }
        };

        if let Err(e) = task.await {
            warn!("ACP agent task error: {e:?}");
        }

        Ok(PromptResponse::new(stop_reason))
    }

    async fn cancel(&self, args: CancelNotification) -> AcpResult<()> {
        let session_id = args.session_id.to_string();
        debug!("ACP cancel: session={session_id}");

        if let Some(entry) = self.get_session(&session_id) {
            let mut guard = entry.cancel_tx.lock().await;
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        Ok(())
    }

    async fn set_session_mode(
        &self,
        args: SetSessionModeRequest,
    ) -> AcpResult<SetSessionModeResponse> {
        let session_id = args.session_id.to_string();
        debug!(
            "ACP set_session_mode: session={session_id} mode={:?}",
            args.mode_id
        );

        let entry = self
            .get_session(&session_id)
            .ok_or_else(Error::invalid_params)?;

        let new_mode = acp_mode_id_to_sven_mode(&args.mode_id);
        *entry.mode_lock.lock().await = new_mode;

        Ok(SetSessionModeResponse::new())
    }
}
