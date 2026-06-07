//! Extended state.
//!
//! The finite state says *where* the machine is; the [`Context`] says *what is
//! known*. Keeping mutable knowledge out of the state enum is what keeps the
//! number of states finite and the dispatch logic readable (UML "extended state
//! variables"). The kernel treats most of this as opaque key/value data so it
//! stays domain-agnostic; the only fields the kernel itself interprets are the
//! approval/permission fields consulted by the permission gate.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::audit::{AuditRecord, ToolAuditRecord};
use crate::ids::ApprovalId;
use crate::permissions::ToolCapability;

/// A pending request for human approval, recorded while the machine waits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// The approval being awaited.
    pub approval_id: ApprovalId,
    /// The capability it would grant.
    pub capability: ToolCapability,
    /// Human-readable description.
    pub description: String,
}

/// Safety-related flags.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SafetyState {
    /// Set when the user cancelled; guards can consult this to bail out.
    pub cancelled: bool,
    /// Count of consecutive failures, used to trip recovery/abort guards.
    pub consecutive_failures: u32,
}

/// Tracks which dangerous capabilities the human has granted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionState {
    /// Capabilities the human has explicitly approved this session.
    pub granted_capabilities: HashSet<ToolCapability>,
}

/// The machine's accumulated knowledge.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    /// Opaque domain facts (goal, problem statement, constraints, backlog, ...).
    /// Kept generic so the kernel never needs to understand the domain.
    pub facts: serde_json::Map<String, Value>,
    /// Latest opaque tool observations, keyed by an arbitrary domain key.
    pub tool_results: serde_json::Map<String, Value>,
    /// Retry counters keyed by an arbitrary label (e.g. a task id or phase).
    pub retry_counters: HashMap<String, u32>,
    /// Created checkpoint labels, most-recent last.
    pub checkpoints: Vec<String>,
    /// The approval currently being awaited, if any.
    pub pending_approval: Option<PendingApproval>,
    /// Safety flags.
    pub safety: SafetyState,
    /// Granted-capability tracking consulted by the permission gate.
    pub permissions: PermissionState,
    /// Append-only audit trail of every dispatch (the event-sourcing spine).
    pub audit: Vec<AuditRecord>,
    /// Per-tool-call audit records (start, denied, approval-required, result).
    ///
    /// Separate from the dispatch audit so tool I/O is auditable independently
    /// of machine transitions.  Does not participate in `replay` (replay only
    /// replays state transitions).
    pub tool_audit: Vec<ToolAuditRecord>,
}

impl Context {
    /// Creates an empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a fact under `key`.
    pub fn set_fact(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.facts.insert(key.into(), value.into());
    }

    /// Reads a fact.
    #[must_use]
    pub fn fact(&self, key: &str) -> Option<&Value> {
        self.facts.get(key)
    }

    /// Records a pending approval and remembers the capability it concerns.
    pub fn set_pending_approval(&mut self, pending: PendingApproval) {
        self.pending_approval = Some(pending);
    }

    /// Grants a capability (typically in response to `HumanApproved`). Clears a
    /// matching pending approval if present.
    pub fn grant(&mut self, capability: ToolCapability) {
        self.permissions.granted_capabilities.insert(capability);
        if self
            .pending_approval
            .as_ref()
            .is_some_and(|p| p.capability == capability)
        {
            self.pending_approval = None;
        }
    }

    /// Resolves a pending approval by id, granting its capability. Returns the
    /// granted capability, or `None` if the id did not match.
    pub fn approve(&mut self, approval_id: ApprovalId) -> Option<ToolCapability> {
        match self.pending_approval.take() {
            Some(p) if p.approval_id == approval_id => {
                self.permissions.granted_capabilities.insert(p.capability);
                Some(p.capability)
            }
            other => {
                self.pending_approval = other;
                None
            }
        }
    }

    /// `true` if `capability` has been granted by the human.
    #[must_use]
    pub fn has_granted(&self, capability: ToolCapability) -> bool {
        self.permissions.granted_capabilities.contains(&capability)
    }

    /// Increments and returns the retry counter under `key`.
    pub fn bump_retry(&mut self, key: impl Into<String>) -> u32 {
        let entry = self.retry_counters.entry(key.into()).or_insert(0);
        *entry += 1;
        *entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_matching_id_grants_capability() {
        let mut ctx = Context::new();
        let id = ApprovalId::new();
        ctx.set_pending_approval(PendingApproval {
            approval_id: id,
            capability: ToolCapability::ExecuteShell,
            description: "run build".into(),
        });
        assert!(!ctx.has_granted(ToolCapability::ExecuteShell));
        assert_eq!(ctx.approve(id), Some(ToolCapability::ExecuteShell));
        assert!(ctx.has_granted(ToolCapability::ExecuteShell));
        assert!(ctx.pending_approval.is_none());
    }

    #[test]
    fn approve_wrong_id_keeps_pending() {
        let mut ctx = Context::new();
        ctx.set_pending_approval(PendingApproval {
            approval_id: ApprovalId::new(),
            capability: ToolCapability::ExecuteShell,
            description: "x".into(),
        });
        assert_eq!(ctx.approve(ApprovalId::new()), None);
        assert!(ctx.pending_approval.is_some());
    }

    #[test]
    fn retry_counter_increments() {
        let mut ctx = Context::new();
        assert_eq!(ctx.bump_retry("build"), 1);
        assert_eq!(ctx.bump_retry("build"), 2);
        assert_eq!(ctx.bump_retry("test"), 1);
    }
}
