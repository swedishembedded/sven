//! Audit records and event-sourcing replay.
//!
//! Every dispatch appends exactly one [`AuditRecord`] to the
//! [`Context`](crate::context::Context). The records form an append-only log
//! that is the system's source of truth: given the recorded **input events**,
//! [`replay`] deterministically reconstructs the machine's state, because the
//! dispatch engine is pure (no I/O; effects are returned, not executed).

use serde::{Deserialize, Serialize};

use crate::context::{Context, Principal};
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
    /// Tenant that owned the session at dispatch time, if a
    /// [`Principal`] was set on the context. `None` for local sessions
    /// and for records serialized before principals existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Actor that drove the session at dispatch time (see `tenant_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
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
            tenant_id: None,
            actor_id: None,
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
            tenant_id: None,
            actor_id: None,
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
            tenant_id: None,
            actor_id: None,
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
            tenant_id: None,
            actor_id: None,
        }
    }

    /// Stamps the owning principal's tenant and actor ids into this record.
    /// A `None` principal leaves the record unattributed (local session).
    pub fn stamp_principal(&mut self, principal: Option<&Principal>) {
        if let Some(p) = principal {
            self.tenant_id = Some(p.tenant_id.clone());
            self.actor_id = Some(p.actor_id.clone());
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
    /// Tenant that owned the session when the call was classified, if a
    /// [`Principal`] was set on the context. `None` for local sessions and
    /// for records serialized before principals existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Actor that drove the session when the call was classified (see
    /// `tenant_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
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
            tenant_id: None,
            actor_id: None,
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
            tenant_id: None,
            actor_id: None,
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
            tenant_id: None,
            actor_id: None,
        }
    }

    /// Stamps the owning principal's tenant and actor ids into this record.
    /// A `None` principal leaves the record unattributed (local session).
    pub fn stamp_principal(&mut self, principal: Option<&Principal>) {
        if let Some(p) = principal {
            self.tenant_id = Some(p.tenant_id.clone());
            self.actor_id = Some(p.actor_id.clone());
        }
    }
}

/// Deterministically reconstructs a machine's state and context from a
/// recorded event log.
///
/// `factory` builds a fresh machine instance; the returned [`Hsm`] has been
/// initialized and then advanced by replaying `events` in order, and the
/// returned [`Context`] is the one that replay accumulated - the audit trail,
/// granted capabilities and domain facts the machine built up along the way.
/// Lifecycle signals in the log are skipped (they are internal to the engine
/// and are regenerated by `init`/`dispatch`). Because dispatch is pure, the
/// result is identical to a live machine that processed the same events.
///
/// Replaying costs O(`events`). To resume a machine for a single further step,
/// prefer [`Hsm::snapshot`] / [`Hsm::restore`], which cost O(1).
#[must_use]
pub fn replay<M, F>(factory: F, events: &[Event]) -> (Hsm<M>, Context)
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
    (hsm, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_record_without_principal_fields_deserializes() {
        // Records serialized before tenant/actor stamping existed must load.
        let json = r#"{
            "from_state": "Idle",
            "to_state": "Busy",
            "event": "UserMessage",
            "effects": [],
            "rationale": null,
            "outcome": "Transition",
            "error": null
        }"#;
        let record: AuditRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.tenant_id, None);
        assert_eq!(record.actor_id, None);
    }

    #[test]
    fn stamp_principal_sets_tenant_and_actor() {
        let mut record = AuditRecord::ignored("Idle", EventKind::UserMessage);
        record.stamp_principal(None);
        assert_eq!(record.tenant_id, None);
        assert_eq!(record.actor_id, None);

        let principal = Principal::new("acme", "alice");
        record.stamp_principal(Some(&principal));
        assert_eq!(record.tenant_id.as_deref(), Some("acme"));
        assert_eq!(record.actor_id.as_deref(), Some("alice"));
    }

    #[test]
    fn unstamped_record_serializes_without_principal_keys() {
        // `skip_serializing_if` keeps legacy logs byte-compatible.
        let record = AuditRecord::ignored("Idle", EventKind::UserMessage);
        let json = serde_json::to_value(&record).unwrap();
        assert!(json.get("tenant_id").is_none());
        assert!(json.get("actor_id").is_none());
    }
}
