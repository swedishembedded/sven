// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Frontend-side mirror of the node/cloud control protocol.
//!
//! The node (`sven-node`) and the cloud control plane speak the same
//! JSON-over-WebSocket protocol: operators send `ControlCommand`s and receive
//! a broadcast stream of `ControlEvent`s. `sven-frontend` sits *below*
//! `sven-node` in the crate graph, so it cannot import the canonical types
//! from `sven_node::control::protocol`; instead this module carries a
//! serialization-compatible mirror of the subset the frontends use.
//!
//! Both the single-session node bridge ([`crate::node_agent`]) and the
//! operator console ([`crate::operator`]) share these types — the wire
//! protocol is defined exactly once on the frontend side.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Operator → node commands ──────────────────────────────────────────────────

/// Commands a frontend sends to a node or cloud control endpoint.
///
/// Serialization-compatible subset of `sven_node`'s `ControlCommand`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Create (or resume) an agent session.
    NewSession {
        /// Caller-supplied session UUID, echoed back in events.
        id: Uuid,
        /// Agent mode as its lowercase wire name (e.g. `"agent"`).
        mode: String,
        working_dir: Option<String>,
    },
    /// Submit a text message to an active session.
    SendInput { session_id: Uuid, text: String },
    /// Switch the model of an active session (the raw `/model` override, e.g.
    /// `"openrouter/free"`). The remote resolves it against ITS config/keys and
    /// rebuilds the session kernel around the new provider.
    SetModel { session_id: Uuid, model: String },
    /// Cancel a running session gracefully.
    CancelSession { session_id: Uuid },
    /// Approve a tool call waiting for operator confirmation.
    ApproveTool { session_id: Uuid, call_id: String },
    /// Deny a tool call waiting for operator confirmation.
    DenyTool {
        session_id: Uuid,
        call_id: String,
        reason: Option<String>,
    },
    /// Request the current list of sessions.
    ListSessions,
    /// Request the schemas of all tools registered on the node.
    ListTools,
    /// Request the current list of connected peers.
    ListPeers,
}

// ── Node → operator events ────────────────────────────────────────────────────

/// Events received on the control stream.
///
/// Serialization-compatible mirror of `sven_node`'s `ControlEvent`; variants
/// the frontends do not consume deserialize as [`ControlEvent::Unknown`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// A streaming text delta from the model.
    OutputDelta {
        session_id: Uuid,
        delta: String,
        /// `"assistant"` or `"thinking"`.
        role: String,
    },
    /// A complete model turn.
    OutputComplete {
        session_id: Uuid,
        text: String,
        role: String,
    },
    /// The model has requested a tool call.
    ToolCall {
        session_id: Uuid,
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    /// A tool call completed.
    ToolResult {
        session_id: Uuid,
        call_id: String,
        output: String,
        is_error: bool,
    },
    /// A tool call requires operator approval before executing.
    ToolNeedsApproval {
        session_id: Uuid,
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    /// The session's lifecycle state changed (snake_case wire name, e.g.
    /// `"awaiting_approval"`).
    SessionState { session_id: Uuid, state: String },
    /// Response to [`ControlCommand::ListSessions`].
    SessionList { sessions: Vec<SessionInfo> },
    /// A recoverable agent error.
    AgentError {
        session_id: Option<Uuid>,
        message: String,
    },
    /// Node-level error (not session-specific).
    NodeError { code: u32, message: String },
    /// Response to [`ControlCommand::ListTools`].
    ToolList { tools: Vec<ToolInfo> },
    /// Response to [`ControlCommand::ListPeers`].
    PeerList { peers: Vec<PeerInfo> },
    /// Any event variant this frontend does not consume.
    #[serde(other)]
    Unknown,
}

// ── Supporting types ──────────────────────────────────────────────────────────

/// Summary of a session returned by [`ControlEvent::SessionList`].
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    /// Agent mode as its lowercase wire name.
    pub mode: String,
    /// Lifecycle state as its snake_case wire name.
    pub state: String,
    pub working_dir: Option<String>,
    /// ISO-8601 timestamp when the session was created.
    pub created_at: String,
}

/// Tool schema entry returned by [`ControlEvent::ToolList`].
#[derive(Debug, Clone, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A connected peer entry returned by [`ControlEvent::PeerList`].
#[derive(Debug, Clone, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub peer_id: String,
    pub connected: bool,
    pub can_delegate: bool,
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn approve_tool_serializes_to_node_wire_format() {
        let sid = Uuid::new_v4();
        let cmd = ControlCommand::ApproveTool {
            session_id: sid,
            call_id: "call-1".into(),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "approve_tool",
                "session_id": sid.to_string(),
                "call_id": "call-1",
            })
        );
    }

    #[test]
    fn list_sessions_serializes_as_bare_tag() {
        let value = serde_json::to_value(ControlCommand::ListSessions).unwrap();
        assert_eq!(value, json!({ "type": "list_sessions" }));
    }

    #[test]
    fn session_list_deserializes_from_node_wire_format() {
        let sid = Uuid::new_v4();
        let text = json!({
            "type": "session_list",
            "sessions": [{
                "id": sid.to_string(),
                "mode": "agent",
                "state": "awaiting_approval",
                "working_dir": null,
                "created_at": "2026-07-14T12:00:00Z",
            }],
        })
        .to_string();
        let evt: ControlEvent = serde_json::from_str(&text).unwrap();
        match evt {
            ControlEvent::SessionList { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, sid);
                assert_eq!(sessions[0].mode, "agent");
                assert_eq!(sessions[0].state, "awaiting_approval");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn unconsumed_event_variants_map_to_unknown() {
        let text = json!({
            "type": "web_device_updated",
            "device_id": Uuid::new_v4().to_string(),
            "status": "approved",
        })
        .to_string();
        let evt: ControlEvent = serde_json::from_str(&text).unwrap();
        assert!(matches!(evt, ControlEvent::Unknown));
    }
}
