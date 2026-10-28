// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Transport-agnostic operator **control protocol** and its kernel mappings.
//!
//! This crate owns the wire vocabulary spoken between a remote operator and a
//! running Sven agent:
//!
//! * [`ControlCommand`] — operator → agent (submit input, approve/deny a tool,
//!   list sessions/tools, …).
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
//! The crate depends only on `sven-hsm` + `sven-vocab` (plus serde / uuid) —
//! **not** on `sven-tool-registry` or any transport stack.
//! That is what lets `sven-frontend` reuse the protocol and mappings without
//! bloating the interactive binaries.

pub mod remote;

use serde::{Deserialize, Serialize};
use sven_vocab::AgentMode;
use uuid::Uuid;

// ── Operator → Agent commands ─────────────────────────────────────────────────

/// Commands sent by a remote operator to control the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Create (or resume) an agent session.
    ///
    /// Idempotent: for a session the host already runs it changes nothing, so
    /// a controller that cannot know whether the host restarted sends it
    /// before every input. A host that does not run the session yet opens it
    /// with `history` as the conversation so far.
    NewSession {
        /// Caller-supplied session UUID. The node echoes it back in events.
        id: Uuid,
        mode: AgentMode,
        /// Working directory for the agent (absolute path or relative to the
        /// server's CWD). `None` keeps the server's current directory.
        working_dir: Option<String>,
        /// The conversation so far, oldest first; empty for a new one.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<HistoryEntry>,
    },

    /// Submit a text message to an active session.
    SendInput { session_id: Uuid, text: String },

    /// Switch the model of an active session.
    ///
    /// `model` is the raw override string an operator types into `/model`
    /// (e.g. `"openrouter/free"`, `"anthropic/claude-opus-4-6"`, or a bare
    /// provider id). The agent-side service resolves it against ITS OWN
    /// configuration (provider configs + API keys) with
    /// `resolve_model_from_config` (`sven-model-drivers`) and rebuilds the session's
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

    /// Request the slash commands a session on this host can run, custom
    /// ones included. The host answers with [`ControlEvent::SlashCommands`].
    ListSlashCommands,

    /// The session's conversation was archived or deleted: the host ends the
    /// session and forgets what it kept for it.
    CloseSession { session_id: Uuid },
}

// ── Agent → Operator events ───────────────────────────────────────────────────

/// Events emitted by the agent and forwarded to all subscribed operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// A session-scoped [`SessionEvent`](sven_vocab::SessionEvent), forwarded
    /// verbatim from the kernel's outward observation plane.
    ///
    /// Replaces the old flattened `OutputDelta`/`OutputComplete`/`ToolCall`/
    /// `ToolResult`/`AgentError` variants, each a lossy hand-maintained
    /// re-encoding of a `UiEvent`/`AgentEvent` (now unified as
    /// `SessionEvent`) that had already drifted from it — e.g. `ToolResult`
    /// dropped the tool's name entirely, forcing every consumer to fabricate
    /// `tool_name: String::new()`, and the translator's exhaustive-looking
    /// match silently swallowed a dozen variants (todo updates, mode/model
    /// changes, context compaction, the transition trace, ...) that
    /// operators never saw as a result. Forwarding the real typed event
    /// fixes both: every field survives, and every variant reaches
    /// operators, not just the ones a translator happened to special-case.
    Session {
        session_id: Uuid,
        event: sven_vocab::SessionEvent,
    },

    /// A tool call requires operator approval before executing.
    ToolNeedsApproval {
        session_id: Uuid,
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },

    /// A tool call as a person reads it, for a surface that only has the
    /// raw call: what it did (`action`), its title (the tool's own one-line
    /// summary of its arguments: a command line, a path, a query; for a tool
    /// with none, its first short text argument), its category (`file`,
    /// `shell`, `search`, `web`, `system`), and, once it has finished, how
    /// long it ran. Sent after the call's `ToolCallStarted`, and again with
    /// `duration_ms` after its `ToolCallFinished`. Costs the model nothing:
    /// every word comes from arguments it already wrote.
    ToolCallDescribed {
        session_id: Uuid,
        call_id: String,
        /// What the call did, readable: the tool's name as words ("Read
        /// file", "Github: create issue"); for an MCP tool, all there is.
        action: String,
        title: String,
        category: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },

    /// The session's lifecycle state changed.
    SessionState {
        session_id: Uuid,
        state: SessionState,
    },

    /// Response to a `ListSessions` command.
    SessionList { sessions: Vec<SessionInfo> },

    /// Node-level error (not session-specific).
    NodeError { code: u32, message: String },

    /// Response to a [`ControlCommand::ListTools`] request.
    ToolList { tools: Vec<ToolSchemaInfo> },

    /// The slash commands a session on this host can run: in answer to
    /// [`ControlCommand::ListSlashCommands`], and once when the host attaches.
    SlashCommands { commands: Vec<SlashCommandInfo> },

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

/// One conversation turn: replayed in [`ControlEvent::History`], or handed to
/// a host in [`ControlCommand::NewSession`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: HistoryRole,
    /// The turn's text content, rendered flat for display.
    pub text: String,
}

// ── Supporting types ──────────────────────────────────────────────────────────

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

/// One slash command as [`ControlEvent::SlashCommands`] lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandInfo {
    /// The name typed after the slash.
    pub name: String,
    /// One line of help: a custom command's frontmatter `description`.
    pub description: String,
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

// ── Kernel mappings ───────────────────────────────────────────────────────────

/// Wraps a kernel [`UiEvent`](sven_hsm::UiEvent) from the observation bus as
/// a [`ControlEvent::Session`] for broadcasting to operator clients.
///
/// Total (no longer `Option`-returning): every variant is forwarded
/// verbatim, not just the handful a hand-maintained translator special-cased.
/// Session-lifecycle bookkeeping (`SessionState::Completed`/`Cancelled` on a
/// terminal event) is the caller's responsibility — it needs to update
/// server-side session-table state as well as broadcast, which this pure
/// mapping has no access to.
#[must_use]
pub fn ui_event_to_control(ev: sven_hsm::UiEvent, session_id: Uuid) -> ControlEvent {
    ControlEvent::Session {
        session_id,
        event: ev,
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
/// (session and tool management) are handled by a service layer,
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
        ControlCommand::DenyTool {
            call_id, reason, ..
        } => {
            let uuid = Uuid::parse_str(call_id).unwrap_or_else(|_| Uuid::new_v4());
            Some(Event::HumanRejected {
                approval_id: ApprovalId::from_uuid(uuid),
                reason: reason.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ui_text_delta_wraps_as_session_event_with_session_id() {
        let sid = Uuid::new_v4();
        match ui_event_to_control(sven_hsm::UiEvent::TextDelta("hi".into()), sid) {
            ControlEvent::Session { session_id, event } => {
                assert_eq!(session_id, sid);
                assert!(matches!(event, sven_vocab::SessionEvent::TextDelta(d) if d == "hi"));
            }
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[test]
    fn attach_command_and_history_event_round_trip() {
        let sid = Uuid::new_v4();

        let cmd = ControlCommand::Attach { session_id: sid };
        let back: ControlCommand =
            serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
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
        let back: ControlEvent =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
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
                history: Vec::new(),
            }),
            Some(sid)
        );
        assert_eq!(
            control_command_session_id(&ControlCommand::ListSessions),
            None
        );
    }
}
