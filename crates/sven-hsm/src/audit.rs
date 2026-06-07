//! Audit records and event-sourcing replay.
//!
//! Every dispatch appends exactly one [`AuditRecord`] to the
//! [`Context`](crate::context::Context). The records form an append-only log
//! that is the system's source of truth: given the recorded **input events**,
//! [`replay`] deterministically reconstructs the machine's state, because the
//! dispatch engine is pure (no I/O; effects are returned, not executed).

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::dispatch::Hsm;
use crate::effect::{Effect, EffectKind};
use crate::event::{Event, EventKind};
use crate::ids::ToolCallId;
use crate::machine::Machine;
use crate::permissions::ToolCapability;

/// What a dispatch ultimately did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    /// A transition was taken (`from` != `to`, or a composite self-transition).
    Transition,
    /// The event was consumed without a transition (internal transition).
    InternalHandled,
    /// No handler in the hierarchy dealt with the event.
    Ignored,
    /// The dispatch's effects were rejected by the permission gate.
    Rejected,
}

/// A single immutable record of one dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// State label before dispatch.
    pub from_state: String,
    /// State label after dispatch.
    pub to_state: String,
    /// The event that was dispatched.
    pub event: EventKind,
    /// Kinds of effects emitted (payload-free, for compact logging).
    pub effects: Vec<EffectKind>,
    /// Transition rationale, if any.
    pub rationale: Option<String>,
    /// Outcome classification.
    pub outcome: AuditOutcome,
    /// Error message, set only when `outcome == Rejected`.
    pub error: Option<String>,
}

impl AuditRecord {
    /// Builds a record for a transition.
    #[must_use]
    pub fn transition(
        from: impl Into<String>,
        to: impl Into<String>,
        event: EventKind,
        effects: &[Effect],
        rationale: Option<String>,
    ) -> Self {
        Self {
            from_state: from.into(),
            to_state: to.into(),
            event,
            effects: effects.iter().map(Effect::kind).collect(),
            rationale,
            outcome: AuditOutcome::Transition,
            error: None,
        }
    }

    /// Builds a record for an internal (consumed, no transition) dispatch.
    #[must_use]
    pub fn internal(
        state: impl Into<String> + Clone,
        event: EventKind,
        effects: &[Effect],
    ) -> Self {
        let s = state.into();
        Self {
            from_state: s.clone(),
            to_state: s,
            event,
            effects: effects.iter().map(Effect::kind).collect(),
            rationale: None,
            outcome: AuditOutcome::InternalHandled,
            error: None,
        }
    }

    /// Builds a record for an ignored event.
    #[must_use]
    pub fn ignored(state: impl Into<String> + Clone, event: EventKind) -> Self {
        let s = state.into();
        Self {
            from_state: s.clone(),
            to_state: s,
            event,
            effects: Vec::new(),
            rationale: None,
            outcome: AuditOutcome::Ignored,
            error: None,
        }
    }

    /// Builds a record for a dispatch whose effects were rejected by the
    /// permission gate. The effects are recorded for forensics but were
    /// **not** executed.
    #[must_use]
    pub fn rejected(
        state: impl Into<String> + Clone,
        event: EventKind,
        effects: &[Effect],
        error: impl Into<String>,
    ) -> Self {
        let s = state.into();
        Self {
            from_state: s.clone(),
            to_state: s,
            event,
            effects: effects.iter().map(Effect::kind).collect(),
            rationale: None,
            outcome: AuditOutcome::Rejected,
            error: Some(error.into()),
        }
    }
}

/// Lifecycle outcome for a single tool-call execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolAuditOutcome {
    /// Tool was dispatched to the executor.
    Started,
    /// Tool was denied by the permission gate (not executed).
    Denied,
    /// Tool is awaiting human approval.
    ApprovalRequired,
    /// Tool completed successfully.
    Succeeded,
    /// Tool completed with an error.
    Failed,
}

/// A per-tool-call audit entry recording what happened to a single
/// `Effect::CallTool` before and after execution.
///
/// These sit alongside the dispatch-level [`AuditRecord`]s in the context's
/// tool audit log and provide the complete tool I/O trace needed for replay,
/// debugging, and policy review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAuditRecord {
    /// State label at the time this call was classified.
    pub state: String,
    /// The tool-call identifier (matches `Effect::CallTool.call_id`).
    pub call_id: ToolCallId,
    /// Tool name.
    pub name: String,
    /// Capability bucket required.
    pub capability: ToolCapability,
    /// Outcome of classification / execution.
    pub outcome: ToolAuditOutcome,
    /// Error or denial reason; `None` for `Started` / `ApprovalRequired`.
    pub message: Option<String>,
}

impl ToolAuditRecord {
    /// Builds a "started" record (tool dispatched to executor).
    #[must_use]
    pub fn started(
        state: impl Into<String>,
        call_id: ToolCallId,
        name: impl Into<String>,
        capability: ToolCapability,
    ) -> Self {
        Self {
            state: state.into(),
            call_id,
            name: name.into(),
            capability,
            outcome: ToolAuditOutcome::Started,
            message: None,
        }
    }

    /// Builds a "denied" record (permission gate rejected the call).
    #[must_use]
    pub fn denied(
        state: impl Into<String>,
        call_id: ToolCallId,
        name: impl Into<String>,
        capability: ToolCapability,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            state: state.into(),
            call_id,
            name: name.into(),
            capability,
            outcome: ToolAuditOutcome::Denied,
            message: Some(reason.into()),
        }
    }

    /// Builds an "approval-required" record.
    #[must_use]
    pub fn approval_required(
        state: impl Into<String>,
        call_id: ToolCallId,
        name: impl Into<String>,
        capability: ToolCapability,
    ) -> Self {
        Self {
            state: state.into(),
            call_id,
            name: name.into(),
            capability,
            outcome: ToolAuditOutcome::ApprovalRequired,
            message: None,
        }
    }
}

/// Deterministically reconstructs a machine's state from a recorded event log.
///
/// `factory` builds a fresh machine instance; the returned [`Hsm`] has been
/// initialized and then advanced by replaying `events` in order. Lifecycle
/// signals in the log are skipped (they are internal to the engine and are
/// regenerated by `init`/`dispatch`). Because dispatch is pure, the resulting
/// state is identical to a live machine that processed the same events.
#[must_use]
pub fn replay<M, F>(factory: F, events: &[Event]) -> Hsm<M>
where
    M: Machine,
    F: FnOnce() -> M,
{
    let mut hsm = Hsm::new(factory());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    for event in events {
        if event.is_lifecycle() {
            continue;
        }
        hsm.dispatch(event, &mut ctx);
    }
    hsm
}
