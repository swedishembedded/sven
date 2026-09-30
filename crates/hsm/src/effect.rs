//! The kernel effect vocabulary.
//!
//! A machine never performs I/O. Every side effect it wishes to cause is
//! returned as a value of type [`Effect`]; the runtime validates the effects
//! against the [`PermissionPolicy`](crate::permissions::PermissionPolicy) and
//! only then hands them to an
//! `EffectExecutor` (`sven-kernel`). The executor performs the
//! actual I/O on a separate task and feeds results back into the queue as
//! [`Event`](crate::event::Event)s.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use sven_vocab::verify::VerifierSpec;

use crate::ids::{ApprovalId, MachineId, QuestionId, TimerId, ToolCallId};
use crate::permissions::ToolCapability;

/// A requested side effect. Pure data; carries no behaviour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Ask the LLM reasoning service for a typed proposal.
    CallLlm {
        /// Opaque request descriptor (a typed `LlmRequest` in higher crates).
        request: Value,
    },
    /// Invoke a tool. Requires the named [`ToolCapability`] to be permitted in
    /// the current state (and, if dangerous, an existing approval).
    CallTool {
        /// Correlates the eventual `ToolSucceeded`/`ToolFailed` event.
        call_id: ToolCallId,
        /// Tool name.
        name: String,
        /// Capability this tool exercises; checked by the permission gate.
        capability: ToolCapability,
        /// Tool arguments.
        args: Value,
    },
    /// Ask the human a question (non-blocking; the answer arrives as an event).
    AskUser {
        /// Prompt text.
        prompt: String,
    },
    /// Request explicit human approval before a dangerous capability is used.
    RequestHumanApproval {
        /// Identifies this approval so the reply can be matched.
        approval_id: ApprovalId,
        /// Capability the approval would grant.
        capability: ToolCapability,
        /// Human-readable description of what is being approved.
        description: String,
    },
    /// Park a question for a human to answer whenever they get to it - unlike
    /// [`Effect::AskUser`], the caller must not assume an imminent reply.
    /// Requires no capability: asking is never itself dangerous.
    RequestHumanAnswer {
        /// Identifies this question so the reply can be matched.
        question_id: QuestionId,
        /// Matches the [`ToolCallId`] of the pending `CallTool` effect.
        call_id: ToolCallId,
        /// The question text shown to the human.
        prompt: String,
        /// Offered choices, if any (empty for a free-form question).
        options: Vec<String>,
    },
    /// Schedule a one-shot timer that posts `Timeout { timer_id }` when elapsed.
    ScheduleTimeout {
        /// Identifies the timer for later cancellation / matching.
        timer_id: TimerId,
        /// Delay before the timeout fires.
        duration: Duration,
    },
    /// Cancel a previously scheduled timer.
    CancelTimeout {
        /// Timer to cancel.
        timer_id: TimerId,
    },
    /// Persist an audit record to the durable event-sourced log.
    PersistAudit,
    /// Re-enter a domain-internal event into the queue.
    EmitInternal {
        /// Signal name.
        name: String,
        /// Opaque payload.
        payload: Value,
    },
    /// Instantiate a child submachine and route subsequent events to it first.
    InstantiateSubmachine {
        /// Identifier the parent will use to refer to the child.
        machine: MachineId,
        /// Opaque descriptor naming which machine to build (a factory key).
        descriptor: Value,
    },
    /// Evaluate a declarative predicate against the real world and report the
    /// verdict as `Event::VerificationComplete`.
    ///
    /// Grading-out-of-band: this is the *only* way a claimed completion turns
    /// into a verdict. No machine parses model text as a decision here - see
    /// `sven_machines::machines::verified_task`'s module doc.
    Verify {
        /// The predicate to check.
        spec: VerifierSpec,
    },
}

impl Effect {
    /// The payload-free classification of this effect (used in audit records
    /// and coverage assertions).
    #[must_use]
    pub fn kind(&self) -> EffectKind {
        match self {
            Effect::CallLlm { .. } => EffectKind::CallLlm,
            Effect::CallTool { .. } => EffectKind::CallTool,
            Effect::AskUser { .. } => EffectKind::AskUser,
            Effect::RequestHumanApproval { .. } => EffectKind::RequestHumanApproval,
            Effect::RequestHumanAnswer { .. } => EffectKind::RequestHumanAnswer,
            Effect::ScheduleTimeout { .. } => EffectKind::ScheduleTimeout,
            Effect::CancelTimeout { .. } => EffectKind::CancelTimeout,
            Effect::PersistAudit => EffectKind::PersistAudit,
            Effect::EmitInternal { .. } => EffectKind::EmitInternal,
            Effect::InstantiateSubmachine { .. } => EffectKind::InstantiateSubmachine,
            Effect::Verify { .. } => EffectKind::Verify,
        }
    }

    /// The capability this effect exercises, if any. An effect that touches
    /// the outside world through a tool reports its capability so the
    /// permission gate can decide whether it is allowed in the current state.
    #[must_use]
    pub fn required_capability(&self) -> Option<ToolCapability> {
        match self {
            Effect::CallTool { capability, .. } => Some(*capability),
            _ => None,
        }
    }
}

/// Payload-free discriminant of [`Effect`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum EffectKind {
    /// See [`Effect::CallLlm`].
    CallLlm,
    /// See [`Effect::CallTool`].
    CallTool,
    /// See [`Effect::AskUser`].
    AskUser,
    /// See [`Effect::RequestHumanApproval`].
    RequestHumanApproval,
    /// See [`Effect::RequestHumanAnswer`].
    RequestHumanAnswer,
    /// See [`Effect::ScheduleTimeout`].
    ScheduleTimeout,
    /// See [`Effect::CancelTimeout`].
    CancelTimeout,
    /// See [`Effect::PersistAudit`].
    PersistAudit,
    /// See [`Effect::EmitInternal`].
    EmitInternal,
    /// See [`Effect::InstantiateSubmachine`].
    InstantiateSubmachine,
    /// See [`Effect::Verify`].
    Verify,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_effect_reports_its_capability() {
        let e = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "shell".into(),
            capability: ToolCapability::ExecuteShell,
            args: Value::Null,
        };
        assert_eq!(e.kind(), EffectKind::CallTool);
        assert_eq!(e.required_capability(), Some(ToolCapability::ExecuteShell));
    }

    #[test]
    fn benign_effect_has_no_capability() {
        assert_eq!(Effect::PersistAudit.required_capability(), None);
    }
}
