// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! `ControlService` — the hub that connects remote operators to the local agent.
//!
//! # Design
//!
//! ```text
//!   P2P handler ──┐
//!                 ├──► mpsc::Sender<ControlCommand> ──► ControlService
//!   WS handler  ──┘                                          │
//!   Slack ────────┘                               owns Agent │
//!                                                 runs turns │
//!   P2P handler ◄──┐                                        │
//!                  ├── broadcast::Receiver<ControlEvent> ◄──┘
//!   WS handler  ◄──┘
//!   Slack ────────┘
//!
//!   spawned task ──► completion_tx ──► service (updates session.state)
//! ```
//!
//! Multiple transport handlers send commands via a shared mpsc channel.
//! The service processes them **sequentially** (the agent is not re-entrant)
//! and broadcasts resulting events to all subscribed operators via a
//! `broadcast` channel.
//!
//! When an agent run completes, the bridge task sends the `session_id` to an
//! internal `completion_tx` so the service can mark the session as `Completed`
//! in its HashMap — making the session available for the next `SendInput`.
//!
//! # Usage
//!
//! ```rust,no_run
//! # use sven_node::control::service::{ControlService, AgentHandle};
//! # use sven_node::control::protocol::{ControlCommand, ControlEvent, SessionState};
//! # use sven_core::Agent;
//! # use uuid::Uuid;
//! # async fn example(agent: Agent) {
//! // Construct the service and get a cheap clone-able handle.
//! let (service, handle) = ControlService::new(agent);
//!
//! // Spawn the service loop.
//! tokio::spawn(service.run());
//!
//! // Transport handlers (P2P, WS, Slack) all hold a clone of `handle`.
//! let session_id = Uuid::new_v4();
//!
//! handle.send(ControlCommand::NewSession {
//!     id: session_id,
//!     mode: sven_config::AgentMode::Agent,
//!     working_dir: None,
//! }).await.unwrap();
//!
//! // Subscribe to events.
//! let mut events = handle.subscribe();
//! while let Ok(ev) = events.recv().await {
//!     match ev {
//!         ControlEvent::OutputDelta { delta, .. } => print!("{delta}"),
//!         ControlEvent::SessionState { state: SessionState::Completed, .. } => break,
//!         _ => {}
//!     }
//! }
//! # }
//! ```

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::Utc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use sven_config::AgentMode;
use sven_core::{Agent, AgentEvent};

use super::protocol::{ControlCommand, ControlEvent, SessionInfo, SessionState};

// ── Public API ────────────────────────────────────────────────────────────────

/// Cheap-to-clone handle to the running `ControlService`.
///
/// All transport handlers (P2P, WebSocket, Slack) hold one of these.
#[derive(Clone)]
pub struct AgentHandle {
    cmd_tx: mpsc::Sender<(ControlCommand, Option<oneshot::Sender<ControlEvent>>)>,
    event_tx: broadcast::Sender<ControlEvent>,
}

impl AgentHandle {
    /// Send a command and optionally await a single-event reply.
    pub async fn send(&self, cmd: ControlCommand) -> anyhow::Result<()> {
        self.cmd_tx
            .send((cmd, None))
            .await
            .map_err(|_| anyhow::anyhow!("control service has shut down"))
    }

    /// Subscribe to the broadcast event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<ControlEvent> {
        self.event_tx.subscribe()
    }
}

// ── Session bookkeeping ───────────────────────────────────────────────────────

struct Session {
    id: Uuid,
    mode: AgentMode,
    state: SessionState,
    working_dir: Option<PathBuf>,
    created_at: String,
    /// Sender half of the cancel channel for the current agent run.
    cancel_tx: Option<oneshot::Sender<()>>,
    /// Pending tool approvals: call_id → oneshot to unblock the tool.
    pending_approvals: HashMap<String, oneshot::Sender<ApprovalDecision>>,
}

#[derive(Debug)]
pub enum ApprovalDecision {
    Approved,
    Denied { reason: Option<String> },
}

impl Session {
    fn new(id: Uuid, mode: AgentMode, working_dir: Option<PathBuf>) -> Self {
        Self {
            id,
            mode,
            state: SessionState::Idle,
            working_dir,
            created_at: Utc::now().to_rfc3339(),
            cancel_tx: None,
            pending_approvals: HashMap::new(),
        }
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            mode: self.mode,
            state: self.state.clone(),
            working_dir: self.working_dir.as_ref().map(|p| p.display().to_string()),
            created_at: self.created_at.clone(),
        }
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

/// The core control service. Owns the agent and processes operator commands.
pub struct ControlService {
    agent: Arc<Mutex<Agent>>,
    cmd_rx: mpsc::Receiver<(ControlCommand, Option<oneshot::Sender<ControlEvent>>)>,
    /// Internal completion notifications from spawned agent tasks.
    /// When an agent run finishes, the task sends the session UUID here so
    /// the service can mark the session as `Completed` in its HashMap.
    completion_rx: mpsc::Receiver<Uuid>,
    completion_tx: mpsc::Sender<Uuid>,
    event_tx: broadcast::Sender<ControlEvent>,
    sessions: HashMap<Uuid, Session>,
}

impl ControlService {
    /// Construct the service with a mock agent (for unit tests only).
    ///
    /// Uses `sven_model::MockProvider` which echoes user input back without
    /// making any network calls.  The tests only exercise the service layer
    /// (session state management) and may optionally trigger real mock agent
    /// runs for integration-level tests.
    #[cfg(test)]
    pub fn new_for_test() -> (Self, AgentHandle) {
        use sven_core::AgentRuntimeContext;
        use sven_tools::{ReadFileTool, ToolRegistry};

        let mut registry = ToolRegistry::new();
        registry.register(ReadFileTool);

        let model = sven_model::MockProvider::default();
        let config = std::sync::Arc::new(sven_config::AgentConfig::default());
        let mode = std::sync::Arc::new(tokio::sync::Mutex::new(sven_config::AgentMode::Agent));
        let (_, tool_rx) = tokio::sync::mpsc::channel(1);

        let agent = Agent::new(
            std::sync::Arc::new(model),
            std::sync::Arc::new(registry),
            config,
            AgentRuntimeContext::default(),
            mode,
            tool_rx,
            8192,
        );
        Self::new(agent)
    }

    /// Construct the service and return a cheap [`AgentHandle`] to it.
    ///
    /// The handle must be cloned and distributed to transport handlers
    /// **before** calling [`ControlService::run`].
    pub fn new(agent: Agent) -> (Self, AgentHandle) {
        // Channel capacity: deep enough to absorb bursts without blocking.
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        // Broadcast capacity: events are small; 1024 is generous.
        let (event_tx, _) = broadcast::channel(1024);
        // Internal completion channel: one slot per concurrent session is fine.
        let (completion_tx, completion_rx) = mpsc::channel(64);

        let handle = AgentHandle {
            cmd_tx,
            event_tx: event_tx.clone(),
        };

        let svc = Self {
            agent: Arc::new(Mutex::new(agent)),
            cmd_rx,
            completion_rx,
            completion_tx,
            event_tx,
            sessions: HashMap::new(),
        };

        (svc, handle)
    }

    /// Run the service event loop. Blocks until the command channel closes.
    pub async fn run(mut self) {
        info!("control service started");
        loop {
            tokio::select! {
                // External operator commands.
                msg = self.cmd_rx.recv() => {
                    let Some((cmd, _reply)) = msg else { break };
                    self.handle_command(cmd).await;
                }
                // Internal: an agent run finished — update session state.
                Some(session_id) = self.completion_rx.recv() => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.state = SessionState::Completed;
                        session.cancel_tx = None;
                    }
                }
            }
        }
        info!("control service stopped");
    }

    async fn handle_command(&mut self, cmd: ControlCommand) {
        match cmd {
            ControlCommand::NewSession {
                id,
                mode,
                working_dir,
            } => {
                self.handle_new_session(id, mode, working_dir).await;
            }
            ControlCommand::SendInput { session_id, text } => {
                self.handle_send_input(session_id, text).await;
            }
            ControlCommand::CancelSession { session_id } => {
                self.handle_cancel(session_id).await;
            }
            ControlCommand::ApproveTool {
                session_id,
                call_id,
            } => {
                self.handle_approve_tool(session_id, call_id, true, None)
                    .await;
            }
            ControlCommand::DenyTool {
                session_id,
                call_id,
                reason,
            } => {
                self.handle_approve_tool(session_id, call_id, false, reason)
                    .await;
            }
            ControlCommand::ListSessions => {
                let sessions: Vec<SessionInfo> = self.sessions.values().map(|s| s.info()).collect();
                self.broadcast(ControlEvent::SessionList { sessions });
            }
            ControlCommand::Subscribe { .. } | ControlCommand::Unsubscribe { .. } => {
                // Handled at the transport layer (subscribe/unsubscribe the
                // broadcast receiver); nothing to do in the service itself.
            }
        }
    }

    async fn handle_new_session(&mut self, id: Uuid, mode: AgentMode, working_dir: Option<String>) {
        if self.sessions.contains_key(&id) {
            self.broadcast(ControlEvent::GatewayError {
                code: 409,
                message: format!("session {id} already exists"),
            });
            return;
        }

        let dir = working_dir.map(PathBuf::from);
        let session = Session::new(id, mode, dir);
        self.sessions.insert(id, session);

        self.broadcast(ControlEvent::SessionState {
            session_id: id,
            state: SessionState::Idle,
        });
        info!(%id, ?mode, "session created");
    }

    async fn handle_send_input(&mut self, session_id: Uuid, text: String) {
        let session = match self.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => {
                self.broadcast(ControlEvent::GatewayError {
                    code: 404,
                    message: format!("session {session_id} not found"),
                });
                return;
            }
        };

        if session.state == SessionState::Running {
            self.broadcast(ControlEvent::GatewayError {
                code: 409,
                message: format!("session {session_id} is already running"),
            });
            return;
        }

        session.state = SessionState::Running;
        info!(%session_id, "session running");
        self.broadcast(ControlEvent::SessionState {
            session_id,
            state: SessionState::Running,
        });

        // Set up cancel channel.
        let (cancel_tx, cancel_rx) = oneshot::channel();
        if let Some(s) = self.sessions.get_mut(&session_id) {
            s.cancel_tx = Some(cancel_tx);
        }

        // Stream agent events through a bounded channel.
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(512);
        let broadcast_tx = self.event_tx.clone();
        let agent = self.agent.clone();

        // Spawn the agent run in a separate task so the service loop remains
        // responsive (can handle CancelSession etc. while agent is running).
        let _agent_task = tokio::spawn({
            let text = text.clone();
            async move {
                let mut agent = agent.lock().await;
                if let Err(e) = agent.submit_with_cancel(&text, event_tx, cancel_rx).await {
                    error!("agent error: {e}");
                }
            }
        });

        // Bridge AgentEvents to ControlEvents in another task.
        let broadcast_tx2 = self.event_tx.clone();
        let completion_tx = self.completion_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = event_rx.recv().await {
                // Log tool calls at info so operators can see what the agent does.
                if let AgentEvent::ToolCallStarted(ref tc) = ev {
                    info!(%session_id, tool=%tc.name, "tool call");
                }
                if let AgentEvent::ToolCallFinished {
                    ref tool_name,
                    is_error,
                    ..
                } = ev
                {
                    if is_error {
                        warn!(%session_id, tool=%tool_name, "tool error");
                    } else {
                        debug!(%session_id, tool=%tool_name, "tool finished");
                    }
                }
                let ctrl_ev = agent_event_to_control(ev, session_id);
                if let Some(ev) = ctrl_ev {
                    let _ = broadcast_tx2.send(ev);
                }
            }
            // Agent run finished: broadcast to operators AND notify the service
            // to update the session state in its HashMap (so future SendInput
            // requests are not rejected with "already running").
            info!(%session_id, "session completed");
            let _ = broadcast_tx.send(ControlEvent::SessionState {
                session_id,
                state: SessionState::Completed,
            });
            let _ = completion_tx.send(session_id).await;
        });
    }

    async fn handle_cancel(&mut self, session_id: Uuid) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            if let Some(tx) = session.cancel_tx.take() {
                let _ = tx.send(());
            }
            session.state = SessionState::Cancelled;
            self.broadcast(ControlEvent::SessionState {
                session_id,
                state: SessionState::Cancelled,
            });
        } else {
            warn!(%session_id, "cancel: session not found");
        }
    }

    async fn handle_approve_tool(
        &mut self,
        session_id: Uuid,
        call_id: String,
        approved: bool,
        reason: Option<String>,
    ) {
        let session = match self.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => {
                warn!(%session_id, "approve_tool: session not found");
                return;
            }
        };

        if let Some(tx) = session.pending_approvals.remove(&call_id) {
            let decision = if approved {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::Denied { reason }
            };
            let _ = tx.send(decision);
        } else {
            warn!(%session_id, %call_id, "approve_tool: no pending approval found");
        }
    }

    fn broadcast(&self, ev: ControlEvent) {
        // Ignore errors: no subscribers is fine (nobody connected yet).
        let _ = self.event_tx.send(ev);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_session_creates_session_in_idle_state() {
        let (svc, handle) = ControlService::new_for_test();
        tokio::spawn(svc.run());

        let mut events = handle.subscribe();
        let session_id = Uuid::new_v4();

        handle
            .send(ControlCommand::NewSession {
                id: session_id,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            })
            .await
            .unwrap();

        // The service broadcasts SessionState::Idle on new session.
        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
            .await
            .expect("no event received")
            .unwrap();

        assert!(matches!(
            ev,
            ControlEvent::SessionState { state: SessionState::Idle, session_id: sid }
            if sid == session_id
        ));
    }

    #[tokio::test]
    async fn duplicate_session_id_returns_error() {
        let (svc, handle) = ControlService::new_for_test();
        tokio::spawn(svc.run());

        let mut events = handle.subscribe();
        let session_id = Uuid::new_v4();

        // First NewSession — succeeds.
        handle
            .send(ControlCommand::NewSession {
                id: session_id,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            })
            .await
            .unwrap();

        // Drain the Idle event.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await;

        // Second NewSession with same id — must return GatewayError(409).
        handle
            .send(ControlCommand::NewSession {
                id: session_id,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            })
            .await
            .unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
            .await
            .expect("no event received")
            .unwrap();

        assert!(matches!(ev, ControlEvent::GatewayError { code: 409, .. }));
    }

    #[tokio::test]
    async fn list_sessions_returns_session_list() {
        let (svc, handle) = ControlService::new_for_test();
        tokio::spawn(svc.run());

        let mut events = handle.subscribe();
        let session_id = Uuid::new_v4();

        handle
            .send(ControlCommand::NewSession {
                id: session_id,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            })
            .await
            .unwrap();

        // Drain Idle event.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await;

        handle.send(ControlCommand::ListSessions).await.unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
            .await
            .expect("no event received")
            .unwrap();

        match ev {
            ControlEvent::SessionList { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, session_id);
                assert_eq!(sessions[0].state, SessionState::Idle);
            }
            other => panic!("expected SessionList, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_nonexistent_session_does_not_panic() {
        let (svc, handle) = ControlService::new_for_test();
        tokio::spawn(svc.run());

        // Should complete without panicking.
        handle
            .send(ControlCommand::CancelSession {
                session_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        // Give the service a moment to process.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn send_input_to_nonexistent_session_returns_error() {
        let (svc, handle) = ControlService::new_for_test();
        tokio::spawn(svc.run());

        let mut events = handle.subscribe();

        handle
            .send(ControlCommand::SendInput {
                session_id: Uuid::new_v4(),
                text: "hello".to_string(),
            })
            .await
            .unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
            .await
            .expect("no event received")
            .unwrap();

        assert!(matches!(ev, ControlEvent::GatewayError { code: 404, .. }));
    }

    #[tokio::test]
    async fn completion_channel_updates_session_state() {
        // This tests the fix: after the agent finishes, the session's state
        // in the HashMap must transition from Running → Completed so that
        // a subsequent SendInput is accepted rather than rejected.

        let (svc, handle) = ControlService::new_for_test();
        let mut events = handle.subscribe();
        let session_id = Uuid::new_v4();

        // Create session.
        let cmd_tx = svc.completion_tx.clone();

        handle
            .send(ControlCommand::NewSession {
                id: session_id,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            })
            .await
            .unwrap();

        tokio::spawn(svc.run());

        // Drain Idle event.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await;

        // Simulate the agent finishing by sending directly to the completion
        // channel.  In production this is done by the bridge task spawned
        // in handle_send_input; here we drive it manually.
        cmd_tx.send(session_id).await.unwrap();

        // Give the service a moment to process the completion.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // ListSessions should now show the session as Completed.
        handle.send(ControlCommand::ListSessions).await.unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
            .await
            .expect("no event received")
            .unwrap();

        match ev {
            ControlEvent::SessionList { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(
                    sessions[0].state,
                    SessionState::Completed,
                    "session must be Completed after the completion channel fires"
                );
            }
            other => panic!("expected SessionList, got {other:?}"),
        }
    }
}

// ── AgentEvent → ControlEvent bridge ─────────────────────────────────────────

fn agent_event_to_control(ev: AgentEvent, session_id: Uuid) -> Option<ControlEvent> {
    match ev {
        AgentEvent::TextDelta(delta) => Some(ControlEvent::OutputDelta {
            session_id,
            delta,
            role: "assistant".to_string(),
        }),
        AgentEvent::TextComplete(text) => Some(ControlEvent::OutputComplete {
            session_id,
            text,
            role: "assistant".to_string(),
        }),
        AgentEvent::ThinkingDelta(delta) => Some(ControlEvent::OutputDelta {
            session_id,
            delta,
            role: "thinking".to_string(),
        }),
        AgentEvent::ThinkingComplete(text) => Some(ControlEvent::OutputComplete {
            session_id,
            text,
            role: "thinking".to_string(),
        }),
        AgentEvent::ToolCallStarted(tc) => Some(ControlEvent::ToolCall {
            session_id,
            call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: tc.args.clone(),
        }),
        AgentEvent::ToolCallFinished {
            call_id,
            tool_name: _,
            output,
            is_error,
        } => Some(ControlEvent::ToolResult {
            session_id,
            call_id,
            output,
            is_error,
        }),
        AgentEvent::Error(msg) => Some(ControlEvent::AgentError {
            session_id: Some(session_id),
            message: msg,
        }),
        // TurnComplete, TokenUsage, ContextCompacted etc. are not forwarded
        // to operators — they're internal agent bookkeeping.
        _ => None,
    }
}
