// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Wire protocol between remote operators and the gateway.
//!
//! All types derive `Serialize`/`Deserialize` and are encoded as:
//! - **CBOR** over P2P (via `ciborium`) — compact binary, no schema needed.
//! - **JSON** over WebSocket — comfortable for browsers and debugging.
//!
//! # Typical session flow
//!
//! ```text
//! Operator                          Gateway / Agent
//!    │                                   │
//!    │── NewSession {id, mode} ─────────►│  SessionState::Idle broadcast
//!    │                                   │
//!    │── SendInput {session_id, text} ───►│  SessionState::Running broadcast
//!    │                                   │  ... OutputDelta × N ...
//!    │◄─ OutputDelta {delta, role} ───────│
//!    │◄─ ToolCall {call_id, tool_name} ───│  (if tool needed)
//!    │◄─ ToolNeedsApproval {call_id} ─────│  (if policy = Ask)
//!    │── ApproveTool {call_id} ──────────►│
//!    │◄─ ToolResult {output} ─────────────│
//!    │◄─ OutputComplete {text} ───────────│
//!    │◄─ SessionState::Completed ─────────│
//!    │                                   │
//!    │── SendInput {session_id, ...} ────►│  session can be reused
//!    │   (accepted — state is Completed) │
//! ```
//!
//! # CBOR codec example
//!
//! ```rust
//! # use sven_node::control::protocol::*;
//! # use uuid::Uuid;
//! let cmd = ControlCommand::SendInput {
//!     session_id: Uuid::new_v4(),
//!     text: "refactor the auth module".to_string(),
//! };
//! let bytes = encode_command(&cmd).unwrap();
//! let back  = decode_command(&bytes).unwrap();
//! assert!(matches!(back, ControlCommand::SendInput { .. }));
//! ```

use serde::{Deserialize, Serialize};
use sven_config::AgentMode;
use uuid::Uuid;

// ── Operator → Agent commands ─────────────────────────────────────────────────

/// Commands sent by a remote operator to control the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Create (or resume) an agent session.
    NewSession {
        /// Caller-supplied session UUID. The gateway echoes it back in events.
        id: Uuid,
        mode: AgentMode,
        /// Working directory for the agent (absolute path or relative to the
        /// server's CWD). `None` keeps the server's current directory.
        working_dir: Option<String>,
    },

    /// Submit a text message to an active session.
    SendInput { session_id: Uuid, text: String },

    /// Cancel a running session gracefully.
    CancelSession { session_id: Uuid },

    /// Approve a tool call that is waiting for operator confirmation.
    ApproveTool { session_id: Uuid, call_id: String },

    /// Deny a tool call that is waiting for operator confirmation.
    DenyTool {
        session_id: Uuid,
        call_id: String,
        reason: Option<String>,
    },

    /// Subscribe to live events for a session.
    ///
    /// The gateway will push `ControlEvent`s on the established stream until
    /// the operator unsubscribes or the connection closes.
    Subscribe { session_id: Uuid },

    /// Stop receiving events for a session.
    Unsubscribe { session_id: Uuid },

    /// Request the current list of sessions.
    ListSessions,
}

// ── Agent → Operator events ───────────────────────────────────────────────────

/// Events emitted by the agent and forwarded to all subscribed operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// A streaming text delta from the model.
    OutputDelta {
        session_id: Uuid,
        /// The text chunk (may be a single character in streaming mode).
        delta: String,
        /// `"assistant"` or `"thinking"`.
        role: String,
    },

    /// A complete model turn (text accumulation finished).
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
        /// Tool arguments as a JSON value.
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

    /// The session's lifecycle state changed.
    SessionState {
        session_id: Uuid,
        state: SessionState,
    },

    /// Response to a `ListSessions` command.
    SessionList { sessions: Vec<SessionInfo> },

    /// A recoverable error occurred (agent continues).
    AgentError {
        session_id: Option<Uuid>,
        message: String,
    },

    /// Gateway-level error (not session-specific).
    GatewayError { code: u32, message: String },
}

// ── Supporting types ──────────────────────────────────────────────────────────

/// Lifecycle state of an agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The session exists but is not currently processing input.
    Idle,
    /// The agent is actively running (model call or tool execution in flight).
    Running,
    /// Waiting for the operator to approve or deny a tool call.
    AwaitingApproval,
    /// The session completed and will accept no more input.
    Completed,
    /// The session was cancelled.
    Cancelled,
}

/// Summary of a session returned by `ListSessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub mode: AgentMode,
    pub state: SessionState,
    pub working_dir: Option<String>,
    /// ISO-8601 timestamp when the session was created.
    pub created_at: String,
}

// ── CBOR codec helpers ────────────────────────────────────────────────────────

/// Encode a `ControlCommand` to CBOR bytes.
pub fn encode_command(cmd: &ControlCommand) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(cmd, &mut buf).map_err(|e| anyhow::anyhow!("CBOR encode: {e}"))?;
    Ok(buf)
}

/// Decode a `ControlCommand` from CBOR bytes.
pub fn decode_command(bytes: &[u8]) -> anyhow::Result<ControlCommand> {
    ciborium::from_reader(bytes).map_err(|e| anyhow::anyhow!("CBOR decode: {e}"))
}

/// Encode a `ControlEvent` to CBOR bytes.
pub fn encode_event(ev: &ControlEvent) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(ev, &mut buf).map_err(|e| anyhow::anyhow!("CBOR encode: {e}"))?;
    Ok(buf)
}

/// Decode a `ControlEvent` from CBOR bytes.
pub fn decode_event(bytes: &[u8]) -> anyhow::Result<ControlEvent> {
    ciborium::from_reader(bytes).map_err(|e| anyhow::anyhow!("CBOR decode: {e}"))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_input_cbor_round_trip() {
        let cmd = ControlCommand::SendInput {
            session_id: Uuid::new_v4(),
            text: "hello world".to_string(),
        };
        let bytes = encode_command(&cmd).unwrap();
        let back = decode_command(&bytes).unwrap();
        match back {
            ControlCommand::SendInput { text, .. } => assert_eq!(text, "hello world"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn output_delta_cbor_round_trip() {
        let id = Uuid::new_v4();
        let ev = ControlEvent::OutputDelta {
            session_id: id,
            delta: "chunk".to_string(),
            role: "assistant".to_string(),
        };
        let bytes = encode_event(&ev).unwrap();
        let back = decode_event(&bytes).unwrap();
        match back {
            ControlEvent::OutputDelta { delta, role, .. } => {
                assert_eq!(delta, "chunk");
                assert_eq!(role, "assistant");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn control_command_json_round_trip() {
        let cmd = ControlCommand::ListSessions;
        let json = serde_json::to_string(&cmd).unwrap();
        let back: ControlCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ControlCommand::ListSessions));
    }

    #[test]
    fn session_state_serializes_as_snake_case() {
        let s = serde_json::to_string(&SessionState::AwaitingApproval).unwrap();
        assert_eq!(s, "\"awaiting_approval\"");
    }
}
