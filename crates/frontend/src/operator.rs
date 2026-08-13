// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Operator console shared by the TUI and GUI frontends.
//!
//! The cloud control plane exposes one control endpoint per tenant, each
//! speaking the same JSON-over-WebSocket protocol as a plain sven node
//! ([`sven_control`]). This module lets a frontend *operate* the platform
//! rather than drive a single chat:
//!
//! - [`OperatorConsole`] — a **pure** projection that folds the tenant-tagged
//!   [`ControlEvent`] stream into a cross-session view: every session of
//!   every tenant, its lifecycle phase, streaming output, running tool, and
//!   pending approvals. Tenant selection narrows the view to one tenant;
//!   [`TenantSelection::All`] shows the whole fleet.
//! - [`operator_console_task`] — the I/O shell: one WebSocket bridge per
//!   [`TenantEndpoint`], commands in ([`OperatorRequest`]), state snapshots
//!   out ([`OperatorSnapshot`]).
//!
//! Both frontends render the same snapshots; neither owns any console logic.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use sven_control::{ControlCommand, ControlEvent, SessionState};

use crate::node_agent::{connect_control_ws, send_cmd};
use crate::types::NodeBackend;

// ── Tenant model ──────────────────────────────────────────────────────────────

/// A tenant the operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantInfo {
    /// The control-plane tenant id (e.g. `"acme"`).
    pub id: String,
    /// Human-readable name shown in pickers and headers.
    pub display_name: String,
}

impl TenantInfo {
    /// A tenant whose display name is its id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            id,
        }
    }
}

/// One tenant's control endpoint: identity plus how to reach its
/// ControlEvent stream.
#[derive(Debug, Clone)]
pub struct TenantEndpoint {
    pub tenant: TenantInfo,
    pub backend: NodeBackend,
}

/// Which tenants the console currently shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TenantSelection {
    /// Cross-tenant view: sessions of every tenant.
    #[default]
    All,
    /// Only sessions of the named tenant.
    Tenant(String),
}

// ── Session view ──────────────────────────────────────────────────────────────

/// A tool call waiting for an operator decision.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call_id: String,
    pub tool_name: String,
    pub args: Value,
}

/// One session row in the cross-session view.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub tenant_id: String,
    pub session_id: Uuid,
    /// Agent mode wire name (empty until a `SessionList` fills it in).
    pub mode: String,
    pub phase: SessionState,
    pub working_dir: Option<String>,
    /// ISO-8601 creation timestamp (empty until a `SessionList` fills it in).
    pub created_at: String,
    /// Assistant text streamed so far in the current turn.
    pub output_buffer: String,
    /// The last complete assistant message.
    pub last_output: Option<String>,
    /// Name of the tool currently executing, if any.
    pub current_tool: Option<String>,
    /// Tool calls waiting for the operator, oldest first.
    pub pending_approvals: Vec<PendingApproval>,
    /// The most recent session-scoped agent error.
    pub last_error: Option<String>,
}

impl SessionView {
    fn new(tenant_id: &str, session_id: Uuid) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            session_id,
            mode: String::new(),
            phase: SessionState::default(),
            working_dir: None,
            created_at: String::new(),
            output_buffer: String::new(),
            last_output: None,
            current_tool: None,
            pending_approvals: Vec::new(),
            last_error: None,
        }
    }
}

// ── Console projection ────────────────────────────────────────────────────────

/// Pure cross-session projection of the tenant-tagged ControlEvent stream.
///
/// Feed it with [`OperatorConsole::apply`]; render it with
/// [`OperatorConsole::visible_sessions`] / [`OperatorConsole::snapshot`].
/// No I/O happens here — the WebSocket bridges live in
/// [`operator_console_task`].
#[derive(Debug, Default)]
pub struct OperatorConsole {
    tenants: Vec<TenantInfo>,
    selection: TenantSelection,
    /// Keyed by `(tenant_id, session_id)` for a deterministic view order.
    sessions: BTreeMap<(String, Uuid), SessionView>,
    last_error: Option<String>,
}

impl OperatorConsole {
    /// A console over the given tenants, starting on the cross-tenant view.
    #[must_use]
    pub fn new(tenants: Vec<TenantInfo>) -> Self {
        Self {
            tenants,
            ..Self::default()
        }
    }

    /// The tenants this console can act on.
    #[must_use]
    pub fn tenants(&self) -> &[TenantInfo] {
        &self.tenants
    }

    /// The current tenant selection.
    #[must_use]
    pub fn selection(&self) -> &TenantSelection {
        &self.selection
    }

    /// Switches the view. Selecting an unknown tenant id is refused and
    /// leaves the current selection untouched; returns whether the selection
    /// was applied.
    pub fn select(&mut self, selection: TenantSelection) -> bool {
        if let TenantSelection::Tenant(id) = &selection {
            if !self.tenants.iter().any(|t| &t.id == id) {
                return false;
            }
        }
        self.selection = selection;
        true
    }

    /// The most recent console-level (non-session) error.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Records a console-level error (connection loss, unknown tenant, ...).
    pub fn note_error(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
    }

    /// Clears the console-level error — e.g. when a tenant bridge
    /// reconnects after the disconnect that produced it.
    pub fn clear_error(&mut self) {
        self.last_error = None;
    }

    /// Looks up one session row.
    #[must_use]
    pub fn session(&self, tenant_id: &str, session_id: Uuid) -> Option<&SessionView> {
        self.sessions.get(&(tenant_id.to_string(), session_id))
    }

    /// The sessions in the current view, ordered by `(tenant, session id)`.
    #[must_use]
    pub fn visible_sessions(&self) -> Vec<&SessionView> {
        self.sessions
            .values()
            .filter(|s| match &self.selection {
                TenantSelection::All => true,
                TenantSelection::Tenant(id) => &s.tenant_id == id,
            })
            .collect()
    }

    /// Total number of pending approvals in the current view.
    #[must_use]
    pub fn pending_approval_count(&self) -> usize {
        self.visible_sessions()
            .iter()
            .map(|s| s.pending_approvals.len())
            .sum()
    }

    /// A renderable copy of the current state.
    #[must_use]
    pub fn snapshot(&self) -> OperatorSnapshot {
        OperatorSnapshot {
            tenants: self.tenants.clone(),
            selection: self.selection.clone(),
            sessions: self.visible_sessions().into_iter().cloned().collect(),
            last_error: self.last_error.clone(),
        }
    }

    /// Folds one event from `tenant_id`'s ControlEvent stream into the view.
    ///
    /// Events for tenants this console does not know are dropped — a
    /// misrouted stream must not leak sessions across tenants.
    pub fn apply(&mut self, tenant_id: &str, event: ControlEvent) {
        if !self.tenants.iter().any(|t| t.id == tenant_id) {
            return;
        }
        match event {
            ControlEvent::SessionList { sessions } => {
                let listed: Vec<Uuid> = sessions.iter().map(|s| s.id).collect();
                self.sessions
                    .retain(|(tenant, id), _| tenant != tenant_id || listed.contains(id));
                for info in sessions {
                    let row = self.row(tenant_id, info.id);
                    row.mode = info.mode.to_string();
                    row.phase = info.state;
                    row.working_dir = info.working_dir;
                    row.created_at = info.created_at;
                }
            }
            ControlEvent::OutputDelta {
                session_id,
                delta,
                role,
            } => {
                let row = self.row(tenant_id, session_id);
                row.phase = SessionState::Running;
                if role != "thinking" {
                    row.output_buffer.push_str(&delta);
                }
            }
            ControlEvent::OutputComplete {
                session_id,
                text,
                role,
            } => {
                if role != "thinking" {
                    let row = self.row(tenant_id, session_id);
                    row.last_output = Some(text);
                    row.output_buffer.clear();
                }
            }
            ControlEvent::ToolCall {
                session_id,
                tool_name,
                ..
            } => {
                let row = self.row(tenant_id, session_id);
                row.phase = SessionState::Running;
                row.current_tool = Some(tool_name);
            }
            ControlEvent::ToolResult {
                session_id,
                call_id,
                ..
            } => {
                let row = self.row(tenant_id, session_id);
                row.current_tool = None;
                row.pending_approvals.retain(|a| a.call_id != call_id);
            }
            ControlEvent::ToolNeedsApproval {
                session_id,
                call_id,
                tool_name,
                args,
            } => {
                let row = self.row(tenant_id, session_id);
                row.phase = SessionState::AwaitingApproval;
                if !row.pending_approvals.iter().any(|a| a.call_id == call_id) {
                    row.pending_approvals.push(PendingApproval {
                        call_id,
                        tool_name,
                        args,
                    });
                }
            }
            ControlEvent::SessionState { session_id, state } => {
                let row = self.row(tenant_id, session_id);
                row.phase = state;
                if row.phase.is_terminal() {
                    row.current_tool = None;
                    row.pending_approvals.clear();
                    if !row.output_buffer.is_empty() {
                        row.last_output = Some(std::mem::take(&mut row.output_buffer));
                    }
                }
            }
            ControlEvent::AgentError {
                session_id,
                message,
            } => match session_id {
                Some(id) => self.row(tenant_id, id).last_error = Some(message),
                None => self.note_error(format!("{tenant_id}: {message}")),
            },
            ControlEvent::NodeError { message, .. } => {
                self.note_error(format!("{tenant_id}: {message}"));
            }
            ControlEvent::ToolList { .. }
            | ControlEvent::PeerList { .. }
            | ControlEvent::ToolCallOutput { .. }
            | ControlEvent::WebDeviceList { .. }
            | ControlEvent::WebDeviceUpdated { .. }
            | ControlEvent::WebDeviceError { .. }
            | ControlEvent::History { .. }
            | ControlEvent::Unknown => {}
        }
    }

    fn row(&mut self, tenant_id: &str, session_id: Uuid) -> &mut SessionView {
        self.sessions
            .entry((tenant_id.to_string(), session_id))
            .or_insert_with(|| SessionView::new(tenant_id, session_id))
    }
}

/// Renderable snapshot published by [`operator_console_task`] after every
/// state change. `sessions` already reflects `selection`.
#[derive(Debug, Clone)]
pub struct OperatorSnapshot {
    pub tenants: Vec<TenantInfo>,
    pub selection: TenantSelection,
    pub sessions: Vec<SessionView>,
    pub last_error: Option<String>,
}

// ── Requests ──────────────────────────────────────────────────────────────────

/// What a frontend asks the operator console to do.
#[derive(Debug, Clone)]
pub enum OperatorRequest {
    /// Change the tenant scope of the view.
    SelectTenant(TenantSelection),
    /// Re-fetch the session list of every tenant.
    RefreshSessions,
    /// Open a session on a tenant (`mode` is the lowercase wire name).
    NewSession {
        tenant_id: String,
        session_id: Uuid,
        mode: String,
    },
    /// Send operator input into a session.
    SendInput {
        tenant_id: String,
        session_id: Uuid,
        text: String,
    },
    /// Approve a pending tool call.
    ApproveTool {
        tenant_id: String,
        session_id: Uuid,
        call_id: String,
    },
    /// Deny a pending tool call.
    DenyTool {
        tenant_id: String,
        session_id: Uuid,
        call_id: String,
        reason: Option<String>,
    },
    /// Cancel a running session.
    CancelSession { tenant_id: String, session_id: Uuid },
}

/// Where a routed request must go.
#[derive(Debug)]
enum Routed {
    /// Update the console's tenant selection (no wire traffic).
    Select(TenantSelection),
    /// Send to every tenant endpoint.
    Broadcast(ControlCommand),
    /// Send to one tenant's endpoint.
    ToTenant {
        tenant_id: String,
        command: ControlCommand,
    },
}

/// Parses the lowercase wire name of an [`OperatorRequest::NewSession`] mode
/// into an [`sven_config::AgentMode`]; unknown names fall back to `Agent`.
fn parse_agent_mode(s: &str) -> sven_config::AgentMode {
    match s {
        "research" => sven_config::AgentMode::Research,
        "plan" => sven_config::AgentMode::Plan,
        "chat" => sven_config::AgentMode::Chat,
        "sdlc" => sven_config::AgentMode::Sdlc,
        _ => sven_config::AgentMode::Agent,
    }
}

/// Maps an [`OperatorRequest`] onto the control protocol. Pure.
fn route_request(request: OperatorRequest) -> Routed {
    match request {
        OperatorRequest::SelectTenant(selection) => Routed::Select(selection),
        OperatorRequest::RefreshSessions => Routed::Broadcast(ControlCommand::ListSessions),
        OperatorRequest::NewSession {
            tenant_id,
            session_id,
            mode,
        } => {
            let mode = parse_agent_mode(&mode);
            Routed::ToTenant {
                tenant_id,
                command: ControlCommand::NewSession {
                    id: session_id,
                    mode,
                    working_dir: None,
                },
            }
        }
        OperatorRequest::SendInput {
            tenant_id,
            session_id,
            text,
        } => Routed::ToTenant {
            tenant_id,
            command: ControlCommand::SendInput { session_id, text },
        },
        OperatorRequest::ApproveTool {
            tenant_id,
            session_id,
            call_id,
        } => Routed::ToTenant {
            tenant_id,
            command: ControlCommand::ApproveTool {
                session_id,
                call_id,
            },
        },
        OperatorRequest::DenyTool {
            tenant_id,
            session_id,
            call_id,
            reason,
        } => Routed::ToTenant {
            tenant_id,
            command: ControlCommand::DenyTool {
                session_id,
                call_id,
                reason,
            },
        },
        OperatorRequest::CancelSession {
            tenant_id,
            session_id,
        } => Routed::ToTenant {
            tenant_id,
            command: ControlCommand::CancelSession { session_id },
        },
    }
}

// ── I/O shell ─────────────────────────────────────────────────────────────────

/// One item flowing from a tenant bridge to the console loop.
enum TenantStreamItem {
    Event {
        tenant_id: String,
        event: ControlEvent,
    },
    /// The bridge (re)established its connection.
    Connected { tenant_id: String },
    /// The bridge lost its connection; it keeps reconnecting with backoff.
    Disconnected {
        tenant_id: String,
        reason: String,
    },
}

/// Background task that drives the operator console.
///
/// Opens one authenticated WebSocket per [`TenantEndpoint`], requests each
/// tenant's session list, folds the merged ControlEvent streams into an
/// [`OperatorConsole`], and publishes an [`OperatorSnapshot`] after every
/// change. `rx` carries the frontend's [`OperatorRequest`]s. Tenant bridges
/// reconnect on their own with exponential backoff; a dropped connection
/// surfaces as a console error until the bridge is back.
///
/// The task ends when `rx` closes or the snapshot receiver is dropped.
pub async fn operator_console_task(
    endpoints: Vec<TenantEndpoint>,
    mut rx: mpsc::Receiver<OperatorRequest>,
    tx: mpsc::Sender<OperatorSnapshot>,
) {
    let tenants: Vec<TenantInfo> = endpoints.iter().map(|e| e.tenant.clone()).collect();
    let mut console = OperatorConsole::new(tenants);

    let (item_tx, mut item_rx) = mpsc::channel::<TenantStreamItem>(256);
    let mut writers: HashMap<String, mpsc::UnboundedSender<String>> = HashMap::new();
    for endpoint in endpoints {
        let tenant_id = endpoint.tenant.id.clone();
        writers.insert(tenant_id, spawn_tenant_bridge(endpoint, item_tx.clone()));
    }
    drop(item_tx);

    for writer in writers.values() {
        let _ = send_cmd(writer, &ControlCommand::ListSessions);
    }
    if tx.send(console.snapshot()).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            request = rx.recv() => {
                let Some(request) = request else { break };
                match route_request(request) {
                    Routed::Select(selection) => {
                        let requested = match &selection {
                            TenantSelection::Tenant(id) => Some(id.clone()),
                            TenantSelection::All => None,
                        };
                        if !console.select(selection) {
                            if let Some(id) = requested {
                                console.note_error(format!("unknown tenant '{id}'"));
                            }
                        }
                    }
                    Routed::Broadcast(command) => {
                        for writer in writers.values() {
                            let _ = send_cmd(writer, &command);
                        }
                    }
                    Routed::ToTenant { tenant_id, command } => match writers.get(&tenant_id) {
                        Some(writer) => {
                            if send_cmd(writer, &command).is_err() {
                                console.note_error(format!(
                                    "tenant '{tenant_id}': control connection closed"
                                ));
                            }
                        }
                        None => console.note_error(format!("unknown tenant '{tenant_id}'")),
                    },
                }
                if tx.send(console.snapshot()).await.is_err() {
                    break;
                }
            }
            item = item_rx.recv() => {
                match item {
                    Some(TenantStreamItem::Event { tenant_id, event }) => {
                        console.apply(&tenant_id, event);
                    }
                    Some(TenantStreamItem::Connected { tenant_id }) => {
                        tracing::debug!(tenant_id, "operator console: tenant bridge connected");
                        console.clear_error();
                    }
                    // The bridge reconnects on its own; keep its writer so
                    // queued commands flow again once it is back.
                    Some(TenantStreamItem::Disconnected { tenant_id, reason }) => {
                        console.note_error(format!(
                            "tenant '{tenant_id}': {reason} (reconnecting)"
                        ));
                    }
                    None => break,
                }
                if tx.send(console.snapshot()).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Delay before the first reconnect attempt of a tenant bridge.
const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling of the reconnect backoff.
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Connects one tenant's control endpoint and pumps its ControlEvent stream
/// into `items`, reconnecting with exponential backoff after connect
/// failures and dropped connections — the console is a long-lived
/// fleet-monitoring surface, so one blip must not blank a tenant until the
/// whole task is restarted. Each (re)connect refreshes the tenant's session
/// list. Returns the sender for outgoing command JSON; the bridge ends when
/// that sender or the console loop is dropped.
fn spawn_tenant_bridge(
    endpoint: TenantEndpoint,
    items: mpsc::Sender<TenantStreamItem>,
) -> mpsc::UnboundedSender<String> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        use futures::SinkExt;

        let tenant_id = endpoint.tenant.id.clone();
        let disconnected = |reason: String| TenantStreamItem::Disconnected {
            tenant_id: endpoint.tenant.id.clone(),
            reason,
        };
        let mut backoff = RECONNECT_INITIAL_BACKOFF;

        loop {
            let ws = match connect_control_ws(
                &endpoint.backend.url,
                &endpoint.backend.token,
                endpoint.backend.insecure,
            )
            .await
            {
                Ok(ws) => ws,
                Err(e) => {
                    if items.send(disconnected(e.to_string())).await.is_err() {
                        return; // console loop is gone
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
                    continue;
                }
            };
            backoff = RECONNECT_INITIAL_BACKOFF;
            if items
                .send(TenantStreamItem::Connected {
                    tenant_id: tenant_id.clone(),
                })
                .await
                .is_err()
            {
                return;
            }
            let (mut sink, mut stream) = ws.split();
            // Refresh the session list on every (re)connect so the view
            // catches up on anything missed while disconnected.
            if let Ok(json) = serde_json::to_string(&ControlCommand::ListSessions) {
                let _ = sink.send(tungstenite::Message::Text(json)).await;
            }

            let reason = loop {
                tokio::select! {
                    json = out_rx.recv() => {
                        let Some(json) = json else { return }; // writer dropped
                        if sink.send(tungstenite::Message::Text(json)).await.is_err() {
                            break "write failed".to_string();
                        }
                    }
                    msg = stream.next() => {
                        match msg {
                            Some(Ok(tungstenite::Message::Text(text))) => {
                                let Ok(event) = serde_json::from_str::<ControlEvent>(&text) else {
                                    continue;
                                };
                                let item = TenantStreamItem::Event {
                                    tenant_id: tenant_id.clone(),
                                    event,
                                };
                                if items.send(item).await.is_err() {
                                    return;
                                }
                            }
                            Some(Ok(tungstenite::Message::Close(_))) | None => {
                                break "connection closed".to_string();
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => break format!("WS recv: {e}"),
                        }
                    }
                }
            };
            if items.send(disconnected(reason)).await.is_err() {
                return;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
        }
    });
    out_tx
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_control::SessionInfo;

    fn console() -> OperatorConsole {
        OperatorConsole::new(vec![TenantInfo::new("acme"), TenantInfo::new("globex")])
    }

    fn info(id: Uuid, state: SessionState) -> SessionInfo {
        SessionInfo {
            id,
            mode: sven_config::AgentMode::Agent,
            state,
            working_dir: None,
            created_at: "2026-07-14T12:00:00Z".into(),
        }
    }

    // ── Tenant selection ──────────────────────────────────────────────────────

    #[test]
    fn default_selection_is_all_tenants() {
        assert_eq!(console().selection(), &TenantSelection::All);
    }

    #[test]
    fn select_known_tenant_narrows_the_view() {
        let mut c = console();
        assert!(c.select(TenantSelection::Tenant("acme".into())));
        assert_eq!(c.selection(), &TenantSelection::Tenant("acme".into()));
    }

    #[test]
    fn select_unknown_tenant_is_refused_and_keeps_selection() {
        let mut c = console();
        assert!(c.select(TenantSelection::Tenant("acme".into())));
        assert!(!c.select(TenantSelection::Tenant("nonexistent".into())));
        assert_eq!(c.selection(), &TenantSelection::Tenant("acme".into()));
    }

    #[test]
    fn select_all_restores_the_cross_tenant_view() {
        let mut c = console();
        assert!(c.select(TenantSelection::Tenant("acme".into())));
        assert!(c.select(TenantSelection::All));
        assert_eq!(c.selection(), &TenantSelection::All);
    }

    // ── Cross-session projection ──────────────────────────────────────────────

    #[test]
    fn session_list_populates_rows_per_tenant() {
        let mut c = console();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(a, SessionState::Running)],
            },
        );
        c.apply(
            "globex",
            ControlEvent::SessionList {
                sessions: vec![info(b, SessionState::Idle)],
            },
        );

        let visible = c.visible_sessions();
        assert_eq!(visible.len(), 2, "cross-tenant view shows both");
        let acme = c.session("acme", a).unwrap();
        assert_eq!(acme.phase, SessionState::Running);
        assert_eq!(acme.mode, "agent");
        assert_eq!(c.session("globex", b).unwrap().phase, SessionState::Idle);
    }

    #[test]
    fn tenant_selection_filters_visible_sessions() {
        let mut c = console();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(a, SessionState::Running)],
            },
        );
        c.apply(
            "globex",
            ControlEvent::SessionList {
                sessions: vec![info(b, SessionState::Running)],
            },
        );

        c.select(TenantSelection::Tenant("globex".into()));
        let visible = c.visible_sessions();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].tenant_id, "globex");
        assert_eq!(visible[0].session_id, b);
    }

    #[test]
    fn session_list_drops_rows_the_tenant_no_longer_reports() {
        let mut c = console();
        let (old, kept) = (Uuid::new_v4(), Uuid::new_v4());
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(old, SessionState::Running), info(kept, SessionState::Running)],
            },
        );
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(kept, SessionState::Completed)],
            },
        );
        assert!(c.session("acme", old).is_none());
        assert_eq!(
            c.session("acme", kept).unwrap().phase,
            SessionState::Completed
        );
    }

    #[test]
    fn session_list_refresh_preserves_streamed_enrichment() {
        let mut c = console();
        let sid = Uuid::new_v4();
        c.apply(
            "acme",
            ControlEvent::OutputComplete {
                session_id: sid,
                text: "done".into(),
                role: "assistant".into(),
            },
        );
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(sid, SessionState::Idle)],
            },
        );
        assert_eq!(
            c.session("acme", sid).unwrap().last_output.as_deref(),
            Some("done")
        );
    }

    #[test]
    fn events_for_unknown_tenants_are_dropped() {
        let mut c = console();
        c.apply(
            "intruder",
            ControlEvent::SessionList {
                sessions: vec![info(Uuid::new_v4(), SessionState::Running)],
            },
        );
        assert!(c.visible_sessions().is_empty());
    }

    #[test]
    fn output_deltas_accumulate_and_complete_replaces_last_output() {
        let mut c = console();
        let sid = Uuid::new_v4();
        for delta in ["hel", "lo"] {
            c.apply(
                "acme",
                ControlEvent::OutputDelta {
                    session_id: sid,
                    delta: delta.into(),
                    role: "assistant".into(),
                },
            );
        }
        {
            let row = c.session("acme", sid).unwrap();
            assert_eq!(row.output_buffer, "hello");
            assert_eq!(row.phase, SessionState::Running);
        }
        c.apply(
            "acme",
            ControlEvent::OutputComplete {
                session_id: sid,
                text: "hello".into(),
                role: "assistant".into(),
            },
        );
        let row = c.session("acme", sid).unwrap();
        assert!(row.output_buffer.is_empty());
        assert_eq!(row.last_output.as_deref(), Some("hello"));
    }

    #[test]
    fn thinking_deltas_do_not_pollute_the_output_buffer() {
        let mut c = console();
        let sid = Uuid::new_v4();
        c.apply(
            "acme",
            ControlEvent::OutputDelta {
                session_id: sid,
                delta: "pondering".into(),
                role: "thinking".into(),
            },
        );
        let row = c.session("acme", sid).unwrap();
        assert!(row.output_buffer.is_empty());
        assert_eq!(row.phase, SessionState::Running);
    }

    #[test]
    fn approval_lifecycle_is_tracked_across_sessions() {
        let mut c = console();
        let sid = Uuid::new_v4();
        c.apply(
            "acme",
            ControlEvent::ToolNeedsApproval {
                session_id: sid,
                call_id: "call-1".into(),
                tool_name: "run_shell".into(),
                args: json!({ "cmd": "ls" }),
            },
        );
        // Duplicate delivery must not double-count.
        c.apply(
            "acme",
            ControlEvent::ToolNeedsApproval {
                session_id: sid,
                call_id: "call-1".into(),
                tool_name: "run_shell".into(),
                args: json!({ "cmd": "ls" }),
            },
        );
        {
            let row = c.session("acme", sid).unwrap();
            assert_eq!(row.phase, SessionState::AwaitingApproval);
            assert_eq!(row.pending_approvals.len(), 1);
            assert_eq!(row.pending_approvals[0].tool_name, "run_shell");
        }
        assert_eq!(c.pending_approval_count(), 1);

        c.apply(
            "acme",
            ControlEvent::ToolResult {
                session_id: sid,
                call_id: "call-1".into(),
                output: "ok".into(),
                is_error: false,
            },
        );
        assert!(c.session("acme", sid).unwrap().pending_approvals.is_empty());
        assert_eq!(c.pending_approval_count(), 0);
    }

    #[test]
    fn terminal_state_clears_activity_and_flushes_the_buffer() {
        let mut c = console();
        let sid = Uuid::new_v4();
        c.apply(
            "acme",
            ControlEvent::OutputDelta {
                session_id: sid,
                delta: "partial".into(),
                role: "assistant".into(),
            },
        );
        c.apply(
            "acme",
            ControlEvent::ToolCall {
                session_id: sid,
                call_id: "call-1".into(),
                tool_name: "read_file".into(),
                args: json!({}),
            },
        );
        c.apply(
            "acme",
            ControlEvent::SessionState {
                session_id: sid,
                state: SessionState::Completed,
            },
        );
        let row = c.session("acme", sid).unwrap();
        assert_eq!(row.phase, SessionState::Completed);
        assert!(row.phase.is_terminal());
        assert!(row.current_tool.is_none());
        assert!(row.pending_approvals.is_empty());
        assert_eq!(row.last_output.as_deref(), Some("partial"));
    }

    #[test]
    fn errors_land_on_the_session_or_the_console() {
        let mut c = console();
        let sid = Uuid::new_v4();
        c.apply(
            "acme",
            ControlEvent::AgentError {
                session_id: Some(sid),
                message: "model timeout".into(),
            },
        );
        assert_eq!(
            c.session("acme", sid).unwrap().last_error.as_deref(),
            Some("model timeout")
        );
        assert!(c.last_error().is_none());

        c.apply(
            "globex",
            ControlEvent::NodeError {
                code: 503,
                message: "overloaded".into(),
            },
        );
        assert_eq!(c.last_error(), Some("globex: overloaded"));
    }

    #[test]
    fn snapshot_reflects_the_selection() {
        let mut c = console();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        c.apply(
            "acme",
            ControlEvent::SessionList {
                sessions: vec![info(a, SessionState::Running)],
            },
        );
        c.apply(
            "globex",
            ControlEvent::SessionList {
                sessions: vec![info(b, SessionState::Running)],
            },
        );
        c.select(TenantSelection::Tenant("acme".into()));

        let snapshot = c.snapshot();
        assert_eq!(snapshot.tenants.len(), 2);
        assert_eq!(snapshot.selection, TenantSelection::Tenant("acme".into()));
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].session_id, a);
    }

    // ── Request routing ───────────────────────────────────────────────────────

    #[test]
    fn refresh_broadcasts_list_sessions() {
        match route_request(OperatorRequest::RefreshSessions) {
            Routed::Broadcast(ControlCommand::ListSessions) => {}
            other => panic!("wrong route: {other:?}"),
        }
    }

    #[test]
    fn approve_routes_to_the_owning_tenant() {
        let sid = Uuid::new_v4();
        let routed = route_request(OperatorRequest::ApproveTool {
            tenant_id: "acme".into(),
            session_id: sid,
            call_id: "call-1".into(),
        });
        match routed {
            Routed::ToTenant { tenant_id, command } => {
                assert_eq!(tenant_id, "acme");
                match command {
                    ControlCommand::ApproveTool {
                        session_id,
                        call_id,
                    } => {
                        assert_eq!(session_id, sid);
                        assert_eq!(call_id, "call-1");
                    }
                    other => panic!("wrong command: {other:?}"),
                }
            }
            other => panic!("wrong route: {other:?}"),
        }
    }

    #[test]
    fn select_tenant_routes_to_the_console_only() {
        let routed = route_request(OperatorRequest::SelectTenant(TenantSelection::All));
        assert!(matches!(routed, Routed::Select(TenantSelection::All)));
    }
}
