// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Transport-agnostic operator **control protocol** and its kernel mappings.
//!
//! This crate owns the wire vocabulary spoken between a remote operator (or a
//! sharing consultant) and a running Sven agent:
//!
//! * [`ControlCommand`] — operator → agent (submit input, approve/deny a tool,
//!   list sessions/tools/peers, …).
//! * [`ControlEvent`] — agent → operator (streamed output, tool activity,
//!   session-state changes, …).
//!
//! plus the two **pure** mappings that connect the protocol to the HSM kernel,
//! extracted so every surface shares one definition:
//!
//! * [`ui_event_to_control`] — a kernel [`UiEvent`](sven_hsm::UiEvent) from the
//!   observation bus → the [`ControlEvent`] broadcast to operators.
//! * [`control_command_to_kernel_event`] — an inbound [`ControlCommand`] →
//!   the [`Event`](sven_hsm::Event) posted onto the kernel queue.
//!
//! The crate depends only on `sven-hsm` + `sven-config` + `sven-vocab` (plus
//! serde / uuid / ciborium) — **not** on `sven-tools` or `sven-node`
//! (libp2p/axum). That is what lets `sven-frontend` reuse the protocol and
//! mappings for the in-process one-tap `/share` bridge without bloating the
//! interactive binaries, while `sven-node` re-exports these types so all
//! existing call sites keep compiling.

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
        /// Caller-supplied session UUID. The node echoes it back in events.
        id: Uuid,
        mode: AgentMode,
        /// Working directory for the agent (absolute path or relative to the
        /// server's CWD). `None` keeps the server's current directory.
        working_dir: Option<String>,
    },

    /// Submit a text message to an active session.
    SendInput { session_id: Uuid, text: String },

    /// Switch the model of an active session.
    ///
    /// `model` is the raw override string an operator types into `/model`
    /// (e.g. `"openrouter/free"`, `"anthropic/claude-opus-4-6"`, or a bare
    /// provider id). The agent-side service resolves it against ITS OWN
    /// configuration (provider configs + API keys) with
    /// [`sven_model::resolve_model_from_config`] and rebuilds the session's
    /// kernel around the new provider while PRESERVING conversation history —
    /// so a remote operator's `/model` actually re-points the server-side
    /// session, not just the local UI. Handled by a service layer (it rebuilds
    /// the kernel), not the kernel queue, so it yields no kernel event.
    SetModel { session_id: Uuid, model: String },

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
    /// The node will push `ControlEvent`s on the established stream until
    /// the operator unsubscribes or the connection closes.
    Subscribe { session_id: Uuid },

    /// Stop receiving events for a session.
    Unsubscribe { session_id: Uuid },

    /// Attach to an existing session and request its conversation history.
    ///
    /// The node replies with a single [`ControlEvent::History`] snapshot of the
    /// conversation so far (so a freshly-connected client — a phone that just
    /// scanned the pairing QR, a second CLI joining a shared "tmux-style"
    /// session — sees the existing transcript), and thereafter the client
    /// receives live events on its subscription. This is the "history on
    /// connect" half of multi-client attach; the live half is the ordinary
    /// broadcast subscription.
    Attach { session_id: Uuid },

    /// Request the current list of sessions.
    ListSessions,

    /// Request the schemas of all tools registered on the node.
    ///
    /// The node responds with a [`ControlEvent::ToolList`] broadcast.
    ListTools,

    /// Execute a single tool directly on the node's tool registry.
    ///
    /// No LLM session is created; the tool runs immediately and the result is
    /// broadcast as [`ControlEvent::ToolCallOutput`] with the same `call_id`.
    CallTool {
        /// Client-supplied correlation ID echoed back in the response.
        call_id: String,
        name: String,
        args: serde_json::Value,
    },

    // ── Web device management ─────────────────────────────────────────────────
    /// Approve a pending web browser device.
    ///
    /// The device transitions from `Pending` → `Approved`, the waiting SSE
    /// stream in the browser fires, and the node starts a PTY session.
    WebDeviceApprove { device_id: Uuid },

    /// Revoke an approved web browser device.
    ///
    /// The device transitions to `Revoked`, any open PTY session is killed,
    /// and the session cookie is effectively invalidated.
    WebDeviceRevoke { device_id: Uuid },

    /// List registered web browser devices.
    WebDeviceList { filter: WebDeviceFilter },

    /// Request the current list of connected peers.
    ///
    /// The node responds with a [`ControlEvent::PeerList`] broadcast.
    ListPeers,

    /// Redeem a one-time pairing token to escalate an unpaired connection.
    ///
    /// This is the ONLY command a `Pending` (unpaired) connection may send. The
    /// transport authenticates the peer's key (Noise/TLS) but authorizes
    /// nothing until this succeeds: the node validates the token, auto-approves
    /// the peer's authenticated identity into its persistent allowlist as
    /// Operator, consumes the token (first-use-wins), and escalates the live
    /// connection. Thereafter the peer reconnects without a token. Handled
    /// entirely at the transport/auth layer — it yields no kernel event.
    Pair { token: String },
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

    /// Node-level error (not session-specific).
    NodeError { code: u32, message: String },

    /// Response to a [`ControlCommand::ListTools`] request.
    ToolList { tools: Vec<ToolSchemaInfo> },

    /// Response to a [`ControlCommand::CallTool`] request.
    ToolCallOutput {
        call_id: String,
        output: String,
        is_error: bool,
    },

    // ── Web device management events ──────────────────────────────────────────
    /// Response to [`ControlCommand::WebDeviceList`].
    WebDeviceList { devices: Vec<WebDeviceSummary> },

    /// Confirmation that a device was approved or revoked.
    WebDeviceUpdated {
        device_id: Uuid,
        /// New status string (`"approved"` or `"revoked"`).
        status: String,
    },

    /// Error response for a web device management command.
    WebDeviceError { message: String },

    // ── Peer management events ────────────────────────────────────────────────
    /// Response to [`ControlCommand::ListPeers`].
    PeerList { peers: Vec<PeerListEntry> },

    /// Reply to [`ControlCommand::Attach`]: the conversation transcript so far.
    ///
    /// Sent once, directly to the attaching client (not broadcast to everyone),
    /// carrying the prior turns so the client can render the existing
    /// conversation before it starts receiving live events.
    History {
        session_id: Uuid,
        entries: Vec<HistoryEntry>,
    },

    /// Any event variant this build doesn't recognize.
    ///
    /// The control protocol crosses a real network boundary (operator client
    /// to node, or node to cloud) where the two ends are not guaranteed to be
    /// built from the same release. Without this catch-all, a client built
    /// against an older protocol version would fail to deserialize (and drop)
    /// every event on the stream the moment the server adds a variant, not
    /// just the new one. Never constructed directly; only reachable through
    /// deserialization of an event this build's `ControlEvent` doesn't define.
    #[serde(other)]
    Unknown,
}

// ── History replay types ──────────────────────────────────────────────────────

/// The speaker of a replayed [`HistoryEntry`].
///
/// A transport-local vocabulary (deliberately NOT `sven_model::Role`) so the
/// control protocol stays free of the model crate; the node renders its
/// conversation store into these when answering an [`ControlCommand::Attach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRole {
    User,
    Assistant,
    Tool,
    System,
}

/// One replayed conversation turn returned in [`ControlEvent::History`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: HistoryRole,
    /// The turn's text content, rendered flat for display.
    pub text: String,
}

// ── Supporting types ──────────────────────────────────────────────────────────

/// A connected peer entry returned in [`ControlEvent::PeerList`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerListEntry {
    /// Human-readable name from the peer's agent card.
    pub name: String,
    /// Libp2p peer ID as a string.
    pub peer_id: String,
    /// Whether the peer is currently connected.
    pub connected: bool,
    /// Whether the peer can accept delegated tasks.
    pub can_delegate: bool,
}

/// Lifecycle state of an agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The session exists but is not currently processing input.
    #[default]
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

impl SessionState {
    /// `true` once the session accepts no more input.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    /// Human-readable label for status lines.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting approval",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Tool schema entry returned by [`ControlEvent::ToolList`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchemaInfo {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

impl From<sven_vocab::ToolSchema> for ToolSchemaInfo {
    fn from(s: sven_vocab::ToolSchema) -> Self {
        Self {
            name: s.name,
            description: s.description,
            parameters: s.parameters,
        }
    }
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

// ── Web device types ──────────────────────────────────────────────────────────

/// Filter for `WebDeviceList`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebDeviceFilter {
    /// All devices regardless of status.
    #[default]
    All,
    /// Only devices awaiting approval.
    Pending,
    /// Only approved devices.
    Approved,
    /// Only revoked devices.
    Revoked,
}

/// Compact device summary returned by `WebDeviceList`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDeviceSummary {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    pub created_at: String,
    pub approved_at: Option<String>,
    pub last_seen: Option<String>,
}

// ── Kernel mappings ───────────────────────────────────────────────────────────

/// Maps a kernel [`UiEvent`](sven_hsm::UiEvent) from the observation bus to a
/// [`ControlEvent`] suitable for broadcasting to WebSocket / P2P / share
/// operator clients.
///
/// Returns `None` for events that have no useful operator representation
/// (e.g. token accounting, transition traces).
pub fn ui_event_to_control(ev: sven_hsm::UiEvent, session_id: Uuid) -> Option<ControlEvent> {
    use sven_hsm::UiEvent;
    match ev {
        UiEvent::TextDelta(delta) => Some(ControlEvent::OutputDelta {
            session_id,
            delta,
            role: "assistant".to_string(),
        }),
        UiEvent::TextComplete(text) => Some(ControlEvent::OutputComplete {
            session_id,
            text,
            role: "assistant".to_string(),
        }),
        UiEvent::ThinkingDelta(delta) => Some(ControlEvent::OutputDelta {
            session_id,
            delta,
            role: "thinking".to_string(),
        }),
        UiEvent::ThinkingComplete(text) => Some(ControlEvent::OutputComplete {
            session_id,
            text,
            role: "thinking".to_string(),
        }),
        UiEvent::ToolStarted {
            call_id,
            name,
            args,
        } => Some(ControlEvent::ToolCall {
            session_id,
            call_id,
            tool_name: name,
            args,
        }),
        UiEvent::ToolFinished {
            call_id,
            output,
            is_error,
            ..
        } => Some(ControlEvent::ToolResult {
            session_id,
            call_id,
            output,
            is_error,
        }),
        UiEvent::Error(message) => Some(ControlEvent::AgentError {
            session_id: Some(session_id),
            message,
        }),
        UiEvent::TurnComplete => Some(ControlEvent::SessionState {
            session_id,
            state: SessionState::Completed,
        }),
        // Token accounting, context compaction, todo updates, mode/model
        // changes, transition traces: not forwarded to operator clients.
        _ => None,
    }
}

/// Maps an inbound [`ControlCommand`] to the kernel [`Event`](sven_hsm::Event)
/// that should be posted onto the session's event queue.
///
/// This is the single source of truth for the steering side of the control
/// protocol:
///
/// * [`ControlCommand::SendInput`] → [`Event::UserMessage`](sven_hsm::Event::UserMessage)
/// * [`ControlCommand::CancelSession`] → [`Event::UserCancelled`](sven_hsm::Event::UserCancelled)
/// * [`ControlCommand::ApproveTool`] → [`Event::HumanApproved`](sven_hsm::Event::HumanApproved)
/// * [`ControlCommand::DenyTool`] → [`Event::HumanRejected`](sven_hsm::Event::HumanRejected)
///
/// The `call_id` in an approve/deny command is parsed as a UUID (the kernel's
/// approval-flow identifier); a non-UUID value falls back to a fresh UUID so
/// the resulting kernel event is always well-formed. All other commands
/// (session/tool/peer management, web devices) are handled by a service layer,
/// not the kernel queue, and yield `None`.
pub fn control_command_to_kernel_event(cmd: &ControlCommand) -> Option<sven_hsm::Event> {
    use sven_hsm::{ApprovalId, Event};
    match cmd {
        ControlCommand::SendInput { text, .. } => Some(Event::UserMessage { text: text.clone() }),
        ControlCommand::CancelSession { .. } => Some(Event::UserCancelled),
        ControlCommand::ApproveTool { call_id, .. } => {
            let uuid = Uuid::parse_str(call_id).unwrap_or_else(|_| Uuid::new_v4());
            Some(Event::HumanApproved {
                approval_id: ApprovalId::from_uuid(uuid),
            })
        }
        ControlCommand::DenyTool { call_id, .. } => {
            let uuid = Uuid::parse_str(call_id).unwrap_or_else(|_| Uuid::new_v4());
            Some(Event::HumanRejected {
                approval_id: ApprovalId::from_uuid(uuid),
            })
        }
        _ => None,
    }
}

/// The session UUID a command targets, when it carries one.
///
/// Used by the in-process share bridge to tag outbound events with the session
/// id the attached consultant expects (it filters the event stream by its own
/// session UUID). Commands with no session target return `None`.
#[must_use]
pub fn control_command_session_id(cmd: &ControlCommand) -> Option<Uuid> {
    match cmd {
        ControlCommand::NewSession { id, .. } => Some(*id),
        ControlCommand::SendInput { session_id, .. }
        | ControlCommand::SetModel { session_id, .. }
        | ControlCommand::CancelSession { session_id }
        | ControlCommand::ApproveTool { session_id, .. }
        | ControlCommand::DenyTool { session_id, .. }
        | ControlCommand::Subscribe { session_id }
        | ControlCommand::Unsubscribe { session_id }
        | ControlCommand::Attach { session_id } => Some(*session_id),
        _ => None,
    }
}

// ── CBOR codec helpers ────────────────────────────────────────────────────────

/// Encode a `ControlCommand` to CBOR bytes.
pub fn encode_command(cmd: &ControlCommand) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    ciborium::into_writer(cmd, &mut buf).map_err(|e| format!("CBOR encode: {e}"))?;
    Ok(buf)
}

/// Decode a `ControlCommand` from CBOR bytes.
pub fn decode_command(bytes: &[u8]) -> Result<ControlCommand, String> {
    ciborium::from_reader(bytes).map_err(|e| format!("CBOR decode: {e}"))
}

/// Encode a `ControlEvent` to CBOR bytes.
pub fn encode_event(ev: &ControlEvent) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    ciborium::into_writer(ev, &mut buf).map_err(|e| format!("CBOR encode: {e}"))?;
    Ok(buf)
}

/// Decode a `ControlEvent` from CBOR bytes.
pub fn decode_event(bytes: &[u8]) -> Result<ControlEvent, String> {
    ciborium::from_reader(bytes).map_err(|e| format!("CBOR decode: {e}"))
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

    #[test]
    fn send_input_maps_to_user_message() {
        let cmd = ControlCommand::SendInput {
            session_id: Uuid::new_v4(),
            text: "do the thing".into(),
        };
        match control_command_to_kernel_event(&cmd) {
            Some(sven_hsm::Event::UserMessage { text }) => assert_eq!(text, "do the thing"),
            other => panic!("expected UserMessage, got {other:?}"),
        }
    }

    #[test]
    fn approve_tool_maps_to_human_approved_with_parsed_uuid() {
        let approval = Uuid::new_v4();
        let cmd = ControlCommand::ApproveTool {
            session_id: Uuid::new_v4(),
            call_id: approval.to_string(),
        };
        match control_command_to_kernel_event(&cmd) {
            Some(sven_hsm::Event::HumanApproved { approval_id }) => {
                assert_eq!(approval_id, sven_hsm::ApprovalId::from_uuid(approval));
            }
            other => panic!("expected HumanApproved, got {other:?}"),
        }
    }

    #[test]
    fn deny_tool_maps_to_human_rejected() {
        let cmd = ControlCommand::DenyTool {
            session_id: Uuid::new_v4(),
            call_id: "not-a-uuid".into(),
            reason: Some("nope".into()),
        };
        assert!(matches!(
            control_command_to_kernel_event(&cmd),
            Some(sven_hsm::Event::HumanRejected { .. })
        ));
    }

    #[test]
    fn management_commands_have_no_kernel_event() {
        assert!(control_command_to_kernel_event(&ControlCommand::ListSessions).is_none());
        assert!(control_command_to_kernel_event(&ControlCommand::ListTools).is_none());
    }

    #[test]
    fn ui_text_delta_maps_to_output_delta_with_session_id() {
        let sid = Uuid::new_v4();
        match ui_event_to_control(sven_hsm::UiEvent::TextDelta("hi".into()), sid) {
            Some(ControlEvent::OutputDelta {
                session_id,
                delta,
                role,
            }) => {
                assert_eq!(session_id, sid);
                assert_eq!(delta, "hi");
                assert_eq!(role, "assistant");
            }
            other => panic!("expected OutputDelta, got {other:?}"),
        }
    }

    #[test]
    fn attach_command_and_history_event_round_trip() {
        let sid = Uuid::new_v4();

        // Attach travels over the P2P (CBOR) transport.
        let cmd = ControlCommand::Attach { session_id: sid };
        let back = decode_command(&encode_command(&cmd).unwrap()).unwrap();
        assert!(matches!(back, ControlCommand::Attach { session_id } if session_id == sid));

        // History carries the replayed conversation to the attaching client.
        let ev = ControlEvent::History {
            session_id: sid,
            entries: vec![
                HistoryEntry {
                    role: HistoryRole::User,
                    text: "hello".into(),
                },
                HistoryEntry {
                    role: HistoryRole::Assistant,
                    text: "hi there".into(),
                },
            ],
        };
        let back = decode_event(&encode_event(&ev).unwrap()).unwrap();
        match back {
            ControlEvent::History {
                session_id,
                entries,
            } => {
                assert_eq!(session_id, sid);
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].role, HistoryRole::User);
                assert_eq!(entries[1].text, "hi there");
            }
            other => panic!("expected History, got {other:?}"),
        }
    }

    #[test]
    fn pair_command_round_trips_and_has_no_kernel_event() {
        let cmd = ControlCommand::Pair {
            token: "one-time-secret".into(),
        };
        // CBOR (P2P transport) round-trip.
        let back = decode_command(&encode_command(&cmd).unwrap()).unwrap();
        assert!(matches!(back, ControlCommand::Pair { token } if token == "one-time-secret"));
        // Pairing is a transport-layer concern, never a kernel event, and has
        // no session target.
        assert!(control_command_to_kernel_event(&cmd).is_none());
        assert_eq!(control_command_session_id(&cmd), None);
    }

    #[test]
    fn attach_command_targets_its_session() {
        let sid = Uuid::new_v4();
        assert_eq!(
            control_command_session_id(&ControlCommand::Attach { session_id: sid }),
            Some(sid)
        );
    }

    #[test]
    fn command_session_id_extracts_target() {
        let sid = Uuid::new_v4();
        assert_eq!(
            control_command_session_id(&ControlCommand::SendInput {
                session_id: sid,
                text: "x".into()
            }),
            Some(sid)
        );
        assert_eq!(
            control_command_session_id(&ControlCommand::NewSession {
                id: sid,
                mode: AgentMode::Agent,
                working_dir: None,
            }),
            Some(sid)
        );
        assert_eq!(
            control_command_session_id(&ControlCommand::ListSessions),
            None
        );
    }
}
