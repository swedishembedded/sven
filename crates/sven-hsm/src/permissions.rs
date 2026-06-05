//! The single permission choke point.
//!
//! Every dispatch runs [`validate_effects_are_allowed`] **before any effect is
//! considered executed**. This is the one place that makes "tool chaos"
//! impossible: a state can only exercise capabilities that its
//! [`PermissionPolicy`] grants, and *dangerous* capabilities additionally
//! require an approval that the human has already granted (recorded in the
//! [`Context`](crate::context::Context)).
//!
//! The policy is keyed by the `Debug` label of the state (`format!("{state:?}")`)
//! so it works for any machine's opaque `StateId` without the kernel needing to
//! know the concrete state type.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::effect::Effect;
use crate::error::MachineError;

/// A coarse capability that a tool/effect exercises. Domain crates map their
/// concrete tools onto these buckets; the kernel reasons only about buckets.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ToolCapability {
    /// Read a file from disk.
    ReadFile,
    /// Write/modify a file on disk.
    WriteFile,
    /// Delete a file.
    DeleteFile,
    /// Execute an arbitrary shell command.
    ExecuteShell,
    /// Access the network.
    NetworkAccess,
    /// Perform a version-control operation (commit, branch, ...).
    GitOperation,
    /// Roll back to a checkpoint (destructive: discards work).
    Rollback,
}

impl ToolCapability {
    /// Capabilities considered inherently dangerous; using them always requires
    /// a granted approval regardless of the per-state allow-set.
    #[must_use]
    pub fn is_inherently_dangerous(self) -> bool {
        matches!(
            self,
            ToolCapability::ExecuteShell | ToolCapability::DeleteFile | ToolCapability::Rollback
        )
    }
}

/// Per-state capability policy plus the global dangerous/approval-required sets.
///
/// Build one with [`PermissionPolicy::builder`].
#[derive(Clone, Debug, Default)]
pub struct PermissionPolicy {
    /// Capabilities allowed per state (keyed by the state's `Debug` label).
    per_state: HashMap<String, HashSet<ToolCapability>>,
    /// Capabilities allowed in every state.
    global_allowed: HashSet<ToolCapability>,
    /// Capabilities that require a granted approval before use.
    approval_required: HashSet<ToolCapability>,
}

impl PermissionPolicy {
    /// Starts an empty policy builder.
    #[must_use]
    pub fn builder() -> PermissionPolicyBuilder {
        PermissionPolicyBuilder::default()
    }

    /// Returns the label used to key a state in the policy.
    #[must_use]
    pub fn state_label<S: Debug>(state: &S) -> String {
        format!("{state:?}")
    }

    /// `true` if `capability` is allowed in `state` (ignoring approval rules).
    fn is_allowed_in<S: Debug>(&self, state: &S, capability: ToolCapability) -> bool {
        if self.global_allowed.contains(&capability) {
            return true;
        }
        self.per_state
            .get(&Self::state_label(state))
            .is_some_and(|caps| caps.contains(&capability))
    }

    /// `true` if `capability` requires a granted approval before it may be used.
    fn requires_approval(&self, capability: ToolCapability) -> bool {
        capability.is_inherently_dangerous() || self.approval_required.contains(&capability)
    }
}

/// Builder for [`PermissionPolicy`].
#[derive(Default)]
pub struct PermissionPolicyBuilder {
    policy: PermissionPolicy,
}

impl PermissionPolicyBuilder {
    /// Allows `caps` in the state identified by `state`'s `Debug` label.
    #[must_use]
    pub fn allow_in<S: Debug>(
        mut self,
        state: S,
        caps: impl IntoIterator<Item = ToolCapability>,
    ) -> Self {
        self.policy
            .per_state
            .entry(PermissionPolicy::state_label(&state))
            .or_default()
            .extend(caps);
        self
    }

    /// Allows `caps` in every state.
    #[must_use]
    pub fn allow_globally(mut self, caps: impl IntoIterator<Item = ToolCapability>) -> Self {
        self.policy.global_allowed.extend(caps);
        self
    }

    /// Marks `caps` as requiring a granted approval before use.
    #[must_use]
    pub fn require_approval(mut self, caps: impl IntoIterator<Item = ToolCapability>) -> Self {
        self.policy.approval_required.extend(caps);
        self
    }

    /// Finalizes the policy.
    #[must_use]
    pub fn build(self) -> PermissionPolicy {
        self.policy
    }
}

/// Validates that every effect produced by a dispatch is permitted in the
/// current state.
///
/// Runs on **every** dispatch before any effect is executed. Two failure modes,
/// both fatal to the batch:
///
/// * [`MachineError::ForbiddenToolCall`] - the capability is not in the
///   state's allow-set.
/// * [`MachineError::HumanApprovalRequired`] - the capability is dangerous /
///   approval-gated and the human has not granted it (see
///   [`Context::has_granted`](crate::context::Context::has_granted)).
///
/// # Errors
///
/// Returns the first violation encountered.
pub fn validate_effects_are_allowed<S: Debug>(
    policy: &PermissionPolicy,
    state: &S,
    effects: &[Effect],
    ctx: &Context,
) -> Result<(), MachineError> {
    for effect in effects {
        let Some(cap) = effect.required_capability() else {
            continue;
        };

        if !policy.is_allowed_in(state, cap) {
            return Err(MachineError::ForbiddenToolCall {
                state: PermissionPolicy::state_label(state),
                capability: cap,
            });
        }

        if policy.requires_approval(cap) && !ctx.has_granted(cap) {
            return Err(MachineError::HumanApprovalRequired {
                state: PermissionPolicy::state_label(state),
                capability: cap,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ToolCallId;
    use serde_json::Value;

    #[derive(Debug)]
    enum S {
        Reading,
        Executing,
    }

    fn read_tool() -> Effect {
        Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "cat".into(),
            capability: ToolCapability::ReadFile,
            args: Value::Null,
        }
    }

    fn shell_tool() -> Effect {
        Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "sh".into(),
            capability: ToolCapability::ExecuteShell,
            args: Value::Null,
        }
    }

    #[test]
    fn allowed_capability_passes() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Reading, [ToolCapability::ReadFile])
            .build();
        let ctx = Context::new();
        assert!(validate_effects_are_allowed(&policy, &S::Reading, &[read_tool()], &ctx).is_ok());
    }

    #[test]
    fn forbidden_capability_is_rejected() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Reading, [ToolCapability::ReadFile])
            .build();
        let ctx = Context::new();
        // ReadFile is not allowed in Executing.
        let err =
            validate_effects_are_allowed(&policy, &S::Executing, &[read_tool()], &ctx).unwrap_err();
        assert!(matches!(err, MachineError::ForbiddenToolCall { .. }));
    }

    #[test]
    fn dangerous_capability_needs_approval() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Executing, [ToolCapability::ExecuteShell])
            .build();

        let mut ctx = Context::new();
        // Allowed in-state but not yet approved -> HumanApprovalRequired.
        let err = validate_effects_are_allowed(&policy, &S::Executing, &[shell_tool()], &ctx)
            .unwrap_err();
        assert!(matches!(err, MachineError::HumanApprovalRequired { .. }));

        // Once granted, it passes.
        ctx.grant(ToolCapability::ExecuteShell);
        assert!(
            validate_effects_are_allowed(&policy, &S::Executing, &[shell_tool()], &ctx).is_ok()
        );
    }
}
