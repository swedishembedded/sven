//! Pure data snapshots the kernel runtime publishes.
//!
//! These types have no tokio dependency of their own — they are what
//! `sven-kernel`'s active-object runtime hands back to observers, not part of
//! the execution engine itself. Kept here (rather than in `sven-kernel`,
//! services tier) so lower-tier crates that only need to *read* a runtime
//! snapshot (e.g. `sven-session-model`'s `MachineProjection`) don't have to
//! depend on the whole tokio execution engine to do it.

use std::sync::{Arc, Mutex};

use crate::audit::{AuditRecord, ToolAuditRecord};
use crate::context::Context;
use crate::machine::Machine;
use crate::Hsm;

/// An observable snapshot of the running machine, published after every
/// dispatch.
#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    /// Current state label.
    pub state_label: String,
    /// `true` once the machine reaches a terminal state.
    pub done: bool,
    /// Most recent permission-gate rejection message, if any.
    pub last_error: Option<String>,
    /// Number of events processed so far.
    pub processed: u64,
}

/// What the consumer task returns when it stops: the final machine and context.
pub struct RuntimeReport<M: Machine> {
    /// The machine in its final state.
    pub hsm: Hsm<M>,
    /// The final extended state (including the full audit trail).
    pub ctx: Context,
}

/// Like [`RuntimeReport`] but for type-erased machines. Returns the final
/// state label and extended context; the machine itself is consumed by the
/// loop.
pub struct ErasedReport {
    /// State label of the machine when it stopped.
    pub state_label: String,
    /// Final extended state (including the full audit trail).
    pub ctx: Context,
}

/// Wraps a state-label string so that `format!("{:?}", label)` returns the
/// label itself — without surrounding quotes — matching the key format used by
/// [`crate::permissions::PermissionPolicy::state_label`].
///
/// Needed by `sven_kernel::ErasedRuntime` to satisfy the `S: Debug` bound of
/// [`crate::permissions::validate_effects_are_allowed`] when the concrete
/// machine type is erased and only a string label is available.
pub struct StateLabel(pub String);

impl std::fmt::Debug for StateLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shared, observable mirror of a running kernel's audit trail.
///
/// The ground-truth audit log lives in [`Context::audit`] /
/// [`Context::tool_audit`], owned by the single consumer task. The runtime
/// mirrors both vectors into this handle after every dispatch — and again
/// immediately *before* executing a dispatch's effects — so out-of-task
/// observers (most importantly an audit-persisting executor servicing
/// [`crate::effect::Effect::PersistAudit`]) can read the records
/// that led up to the effect they are executing.
///
/// Both mirrors are append-only: records are only ever appended, never mutated
/// or removed, so consumers may keep a cursor into the snapshots they take.
#[derive(Clone, Debug, Default)]
pub struct AuditTrailHandle {
    records: Arc<Mutex<Vec<AuditRecord>>>,
    tool_records: Arc<Mutex<Vec<ToolAuditRecord>>>,
}

impl AuditTrailHandle {
    /// Creates an empty trail.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of the dispatch audit records mirrored so far.
    #[must_use]
    pub fn records(&self) -> Vec<AuditRecord> {
        self.records
            .lock()
            .expect("audit trail mutex poisoned")
            .clone()
    }

    /// A snapshot of the per-tool-call audit records mirrored so far.
    #[must_use]
    pub fn tool_records(&self) -> Vec<ToolAuditRecord> {
        self.tool_records
            .lock()
            .expect("audit trail mutex poisoned")
            .clone()
    }

    /// The dispatch audit records mirrored so far, starting at index `start`.
    ///
    /// Because the trail is append-only, a consumer that has already processed
    /// `start` records can fetch just the new suffix instead of cloning the
    /// whole (potentially long-session) vector.
    #[must_use]
    pub fn records_from(&self, start: usize) -> Vec<AuditRecord> {
        let guard = self.records.lock().expect("audit trail mutex poisoned");
        guard[start.min(guard.len())..].to_vec()
    }

    /// The per-tool-call audit records mirrored so far, starting at index
    /// `start` (see [`records_from`](Self::records_from)).
    #[must_use]
    pub fn tool_records_from(&self, start: usize) -> Vec<ToolAuditRecord> {
        let guard = self
            .tool_records
            .lock()
            .expect("audit trail mutex poisoned");
        guard[start.min(guard.len())..].to_vec()
    }

    /// Mirrors `ctx`'s audit vectors into this handle.
    ///
    /// Called by the kernel's consumer task. Both vectors in [`Context`] are
    /// append-only, so a sync normally just appends the new suffix (O(new
    /// records), not O(all records)); if `ctx` is ever shorter than the
    /// mirror (a fresh context reusing a handle), the mirror is replaced.
    pub fn sync_from(&self, ctx: &Context) {
        fn sync_vec<T: Clone + PartialEq>(mirror: &Mutex<Vec<T>>, source: &[T]) {
            if let Ok(mut guard) = mirror.lock() {
                if guard.len() <= source.len() {
                    let cursor = guard.len();
                    guard.extend_from_slice(&source[cursor..]);
                } else {
                    guard.clear();
                    guard.extend_from_slice(source);
                }
            }
        }
        sync_vec(&self.records, &ctx.audit);
        sync_vec(&self.tool_records, &ctx.tool_audit);
    }
}

#[cfg(test)]
mod tests {
    use crate::context::Principal;
    use crate::event::EventKind;
    use crate::ids::ToolCallId;
    use crate::permissions::ToolCapability;

    use super::*;

    #[test]
    fn audit_trail_handle_mirrors_context_vectors() {
        let trail = AuditTrailHandle::new();
        assert!(trail.records().is_empty());
        assert!(trail.tool_records().is_empty());

        let mut ctx = Context::new();
        ctx.principal = Some(Principal::new("acme", "alice"));
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::UserMessage));
        ctx.tool_audit.push(ToolAuditRecord::started(
            "Working",
            ToolCallId::new(),
            "read_file",
            ToolCapability::ReadFile,
        ));

        trail.sync_from(&ctx);

        let records = trail.records();
        assert_eq!(records.len(), 1);
        // Principal stamping from push_audit is preserved in the mirror.
        assert_eq!(records[0].tenant_id.as_deref(), Some("acme"));
        assert_eq!(records[0].actor_id.as_deref(), Some("alice"));
        assert_eq!(trail.tool_records().len(), 1);

        // Re-syncing an extended context only appends.
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::Timeout));
        trail.sync_from(&ctx);
        assert_eq!(trail.records().len(), 2);
        assert_eq!(trail.records()[0], records[0]);
    }

    #[test]
    fn audit_trail_handle_clones_share_state() {
        let trail = AuditTrailHandle::new();
        let clone = trail.clone();

        let mut ctx = Context::new();
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::UserMessage));
        trail.sync_from(&ctx);

        assert_eq!(clone.records().len(), 1, "clone observes the same trail");
    }
}
