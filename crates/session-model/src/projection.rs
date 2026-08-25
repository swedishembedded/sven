// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`MachineProjection`] — UI-relevant snapshot of kernel machine state.
//!
//! The kernel publishes [`sven_hsm::RuntimeStatus`] after every dispatch, but
//! that gives only the current state label. `MachineProjection` enriches it
//! with the information frontends actually need to render the chat UI:
//! whether the machine is waiting for user input or approval, the last
//! assistant response, and a tail of the audit log for debugging.
//!
//! # Sources
//!
//! | Field | Derived from |
//! |-------|-------------|
//! | `mode` | `RuntimeBuilder`'s mode string |
//! | `phase` | `RuntimeStatus::state_label` |
//! | `is_busy` | machine running, not terminal, not waiting |
//! | `is_awaiting_user` | `Effect::AskUser` in flight |
//! | `is_awaiting_approval` | `Effect::RequestHumanApproval` in flight |
//! | `pending_question` | question payload forwarded from `UserExecutor` |
//! | `pending_approval` | approval payload forwarded from `UserExecutor` |
//! | `last_response` | latest `Event::LlmProposedResponse` text |
//! | `audit_tail` | last N [`AuditRecord`]s serialised to JSON |
//!
//! Pure data + derivation only — the broadcast channel that carries these
//! between the kernel task and a frontend is tokio-based plumbing and stays
//! in `sven-frontend` (`ProjectionTx`/`ProjectionRx`/`projection_channel`).

use serde_json::Value;
use sven_hsm::RuntimeStatus;

/// A snapshot of the machine state that the UI layer can render directly.
///
/// The projection is updated on every kernel dispatch via
/// [`MachineProjection::from_status`] and enriched with side-channel data
/// from the `sven_executors::user::UserExecutor` channels when questions or
/// approvals arrive.
#[derive(Clone, Debug, Default)]
pub struct MachineProjection {
    /// The mode string used to select the machine (e.g. `"chat"`, `"sdlc"`).
    pub mode: String,
    /// Human-readable label of the current machine state (e.g. `"Responding"`).
    pub phase: String,
    /// `true` while the machine is processing (not terminal, not waiting).
    pub is_busy: bool,
    /// `true` while the machine is waiting for a user answer.
    pub is_awaiting_user: bool,
    /// `true` while the machine is waiting for a human approval decision.
    pub is_awaiting_approval: bool,
    /// The question payload if the machine issued `Effect::AskUser`.
    pub pending_question: Option<Value>,
    /// The approval payload if the machine issued `Effect::RequestHumanApproval`.
    pub pending_approval: Option<Value>,
    /// The most recent complete LLM response text.
    pub last_response: Option<String>,
    /// Tail of the audit log (last N records, serialised to JSON).
    pub audit_tail: Vec<Value>,
}

impl MachineProjection {
    /// Build a projection from a [`RuntimeStatus`] snapshot.
    ///
    /// Enrichment (questions / approvals / last response / audit) must be
    /// applied separately once those values arrive from the user-executor
    /// channels.
    #[must_use]
    pub fn from_status(status: &RuntimeStatus, mode: &str) -> Self {
        let is_awaiting_user = status.state_label.contains("AwaitUser")
            || status.state_label.contains("Clarify")
            || status.state_label.contains("AskUser");

        let is_awaiting_approval = status.state_label.contains("AwaitApproval")
            || status.state_label.contains("HumanApproval")
            || status.state_label.contains("AwaitHuman");

        let is_busy = !status.done && !is_awaiting_user && !is_awaiting_approval;

        Self {
            mode: mode.to_string(),
            phase: status.state_label.clone(),
            is_busy,
            is_awaiting_user,
            is_awaiting_approval,
            pending_question: None,
            pending_approval: None,
            last_response: None,
            audit_tail: Vec::new(),
        }
    }

    /// Returns `true` if the machine has reached a terminal state.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.phase.contains("Done")
            || self.phase.contains("Complete")
            || self.phase.contains("Completed")
            || self.phase.contains("Failed")
            || self.phase.contains("Cancelled")
    }
}

/// Maps a [`MachineProjection`] to the legacy `SessionState`
/// variants so the node control service can broadcast backwards-compatible
/// events to older clients.
///
/// Called by the node control service's projection-bridge task.
#[must_use]
pub fn projection_to_session_state(proj: &MachineProjection) -> &'static str {
    if proj.is_done() {
        if proj.phase.contains("Cancelled") {
            "Cancelled"
        } else {
            "Completed"
        }
    } else if proj.is_awaiting_approval {
        "AwaitingApproval"
    } else if proj.is_awaiting_user || proj.is_busy {
        "Running"
    } else {
        "Idle"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::RuntimeStatus;

    fn status(label: &str, done: bool) -> RuntimeStatus {
        RuntimeStatus {
            state_label: label.to_string(),
            done,
            last_error: None,
            processed: 0,
        }
    }

    #[test]
    fn from_status_running_machine_is_busy() {
        let proj = MachineProjection::from_status(&status("Responding", false), "chat");
        assert!(proj.is_busy);
        assert!(!proj.is_awaiting_user);
        assert!(!proj.is_awaiting_approval);
        assert!(!proj.is_done());
    }

    #[test]
    fn from_status_await_user_state() {
        let proj = MachineProjection::from_status(&status("AwaitUser", false), "chat");
        assert!(!proj.is_busy);
        assert!(proj.is_awaiting_user);
    }

    #[test]
    fn from_status_terminal_state() {
        let proj = MachineProjection::from_status(&status("Done", true), "chat");
        assert!(!proj.is_busy);
        assert!(proj.is_done());
    }

    #[test]
    fn projection_to_session_state_running() {
        let proj = MachineProjection {
            is_busy: true,
            ..MachineProjection::default()
        };
        assert_eq!(projection_to_session_state(&proj), "Running");
    }
}
