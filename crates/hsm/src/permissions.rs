//! The single permission choke point.
//!
//! Every dispatch runs [`validate_effects_are_allowed`] **before any effect is
//! considered executed**. This is the one place that makes "tool chaos"
//! impossible: a state can only exercise capabilities that its
//! [`PermissionPolicy`] grants. A policy may additionally require a person's
//! approval for some capabilities ([`PermissionPolicy::with_manual_approval`]);
//! such a call runs only once that person has approved that very call
//! (recorded in the [`Context`]). Without it, what
//! the state allows runs without anyone being asked.
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
    /// Evaluate a declarative `VerifierSpec` (`Effect::Verify`) against the
    /// real world.
    ///
    /// Read-only: v1's declarative shapes are read-only
    /// (`FileExists`, `FileHash`, `JsonPredicate`, path-jailed; `HttpPredicate`
    /// is a `GET`). There is no `Command`/`UnitTests` shape, so no process
    /// spawning and no write surface - see the verifier vocabulary's module
    /// doc (`sven_vocab::verify`). Still its own bucket, allow-listed only for
    /// the verified-task machine's `Verifying` state: nothing else in the
    /// system has any reason to trigger a verification.
    RunVerifier,
    /// Drive an external physical/virtual device (e.g. an Android phone over
    /// ADB) through a narrow, typed operation set: screenshot, tap, type,
    /// launch/stop an app.
    ///
    /// Deliberately its own bucket rather than [`ToolCapability::ExecuteShell`]:
    /// the risk profile is different (no shell metacharacter injection, no
    /// filesystem/process access on the controlling host - only the specific
    /// verbs a device-control tool exposes reach the device), and a machine
    /// that drives a device should be able to allow this without also
    /// granting arbitrary shell execution.
    ControlDevice,
    /// Start a child agent run (`Effect::InstantiateSubmachine`).
    ///
    /// Gated like a tool so a state starts children only where its policy
    /// says so, and so a child - whose policy is its parent's narrowed - may
    /// start children of its own only if every run above it could. Read-only
    /// in itself: what a child may do is bounded by the
    /// [`ChildRunContract`](crate::contract::ChildRunContract) it inherits,
    /// not by this bucket.
    SpawnChild,
}

impl ToolCapability {
    /// Every capability, for code that reasons over the whole set.
    pub const ALL: [ToolCapability; 9] = [
        ToolCapability::ReadFile,
        ToolCapability::WriteFile,
        ToolCapability::DeleteFile,
        ToolCapability::ExecuteShell,
        ToolCapability::NetworkAccess,
        ToolCapability::GitOperation,
        ToolCapability::RunVerifier,
        ToolCapability::ControlDevice,
        ToolCapability::SpawnChild,
    ];

    /// `true` for a capability that changes nothing outside the run:
    /// reading files, evaluating a read-only verifier, starting a child
    /// (which is held to its own contract). Manual approval asks about every
    /// other one.
    ///
    /// `NetworkAccess` is not read-only: the bucket also holds MCP tools,
    /// whose effect the kernel cannot tell from their name.
    #[must_use]
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            ToolCapability::ReadFile | ToolCapability::RunVerifier | ToolCapability::SpawnChild
        )
    }
}

/// Per-state capability policy plus the set of capabilities that need a
/// person's approval per call.
///
/// Build one with [`PermissionPolicy::builder`].
#[derive(Clone, Debug, Default)]
pub struct PermissionPolicy {
    /// Capabilities allowed per state (keyed by the state's `Debug` label).
    per_state: HashMap<String, HashSet<ToolCapability>>,
    /// Capabilities allowed in every state.
    global_allowed: HashSet<ToolCapability>,
    /// Capabilities each call of which needs a person's approval first.
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
    #[must_use]
    pub fn allows<S: Debug>(&self, state: &S, capability: ToolCapability) -> bool {
        if self.global_allowed.contains(&capability) {
            return true;
        }
        self.per_state
            .get(&Self::state_label(state))
            .is_some_and(|caps| caps.contains(&capability))
    }

    /// `true` if `capability` is allowed whatever state the machine is in.
    #[must_use]
    pub fn allows_in_every_state(&self, capability: ToolCapability) -> bool {
        self.global_allowed.contains(&capability)
    }

    /// `true` if each call exercising `capability` needs a person's approval
    /// before it runs.
    #[must_use]
    pub fn requires_approval(&self, capability: ToolCapability) -> bool {
        self.approval_required.contains(&capability)
    }

    /// This policy under manual approval: every capability that is not
    /// [read-only](ToolCapability::is_read_only) needs a person's approval
    /// for each call. What the policy allows is unchanged - approval only
    /// ever narrows it.
    #[must_use]
    pub fn with_manual_approval(mut self) -> Self {
        self.approval_required.extend(
            ToolCapability::ALL
                .into_iter()
                .filter(|cap| !cap.is_read_only()),
        );
        self
    }

    /// What `state` may do, as a policy that allows exactly that in every
    /// state, with the same approval requirements.
    ///
    /// This is how a run's authority is handed to another machine: the
    /// receiving machine's states have their own labels, so a per-state
    /// policy keyed by the giver's labels would mean nothing to it.
    #[must_use]
    pub fn ceiling_in<S: Debug>(&self, state: &S) -> Self {
        Self {
            per_state: HashMap::new(),
            global_allowed: ToolCapability::ALL
                .into_iter()
                .filter(|&cap| self.allows(state, cap))
                .collect(),
            approval_required: self.approval_required.clone(),
        }
    }

    /// What every state may do, as a policy: the authority that holds
    /// whichever state the machine is in when nobody can say which one.
    #[must_use]
    pub fn ceiling_in_every_state(&self) -> Self {
        Self {
            per_state: HashMap::new(),
            global_allowed: self.global_allowed.clone(),
            approval_required: self.approval_required.clone(),
        }
    }

    /// The policy that allows a capability in a state only where both
    /// `self` and `other` allow it there, and requires approval for anything
    /// either requires approval for. Never allows more than either side.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let global_allowed: HashSet<ToolCapability> = self
            .global_allowed
            .intersection(&other.global_allowed)
            .copied()
            .collect();
        let per_state = self
            .per_state
            .keys()
            .chain(other.per_state.keys())
            .map(|label| {
                let allowed = |policy: &Self, cap: ToolCapability| {
                    policy.global_allowed.contains(&cap)
                        || policy
                            .per_state
                            .get(label)
                            .is_some_and(|c| c.contains(&cap))
                };
                let caps: HashSet<ToolCapability> = ToolCapability::ALL
                    .into_iter()
                    .filter(|&cap| !global_allowed.contains(&cap))
                    .filter(|&cap| allowed(self, cap) && allowed(other, cap))
                    .collect();
                (label.clone(), caps)
            })
            .filter(|(_, caps)| !caps.is_empty())
            .collect();
        Self {
            per_state,
            global_allowed,
            approval_required: self
                .approval_required
                .union(&other.approval_required)
                .copied()
                .collect(),
        }
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

    /// Marks `caps` as needing a person's approval for each call.
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

/// Outcome of classifying a single [`Effect`] against the policy.
#[derive(Debug, Clone)]
pub enum EffectDisposition {
    /// The effect is permitted; execute it immediately.
    Allowed,
    /// The capability is not in the state's allow-set; synthesize a failure.
    Forbidden(String),
    /// The capability is allowed but the policy asks a person to approve
    /// this call first, and nobody has yet. The kernel emits
    /// [`crate::event::Event::ToolApprovalRequired`] and the machine handles
    /// the approval flow.
    NeedsApproval(ToolCapability),
}

/// Classifies a single [`Effect`] against the policy and context.
///
/// This is the per-effect equivalent of [`validate_effects_are_allowed`].
/// The runtime calls this for each `CallTool` effect individually so it can
/// take graceful per-call actions (execute, deny with `ToolFailed`, or gate
/// with `ToolApprovalRequired`) rather than rejecting the whole batch.
///
/// Non-`CallTool` effects always return [`EffectDisposition::Allowed`] (the
/// all-or-nothing batch check in `validate_effects_are_allowed` still guards
/// those).
pub fn classify<S: Debug>(
    policy: &PermissionPolicy,
    state: &S,
    ctx: &Context,
    effect: &Effect,
) -> EffectDisposition {
    let Some(cap) = effect.required_capability() else {
        return EffectDisposition::Allowed;
    };
    if !policy.allows(state, cap) {
        return EffectDisposition::Forbidden(format!(
            "capability {cap:?} is not permitted in state {:?}",
            PermissionPolicy::state_label(state)
        ));
    }
    if policy.requires_approval(cap) && !approved(ctx, effect) {
        return EffectDisposition::NeedsApproval(cap);
    }
    EffectDisposition::Allowed
}

/// `true` if a person has approved `effect` itself. Only a tool call can be
/// approved: any other effect a policy asks approval for is refused.
fn approved(ctx: &Context, effect: &Effect) -> bool {
    match effect {
        Effect::CallTool { call_id, .. } => ctx.is_call_approved(*call_id),
        _ => false,
    }
}

/// Infers a [`ToolCapability`] from a tool name using naming conventions.
///
/// Used by machines that do not have access to a `ToolRegistry` (e.g.
/// legacy machines that receive `LlmProposedToolCall { name }` and must emit
/// `Effect::CallTool` with a capability).  The mapping is best-effort;
/// `ToolRegistry::capability_of` (which calls `Tool::kernel_capability`) is
/// authoritative when a registry is available. A name no convention covers
/// maps to [`ToolCapability::NetworkAccess`].
#[must_use]
pub fn capability_for_tool_name(name: &str) -> ToolCapability {
    known_capability_for_tool_name(name).unwrap_or(ToolCapability::NetworkAccess)
}

/// The capability a tool name's naming convention names, or `None` for a
/// name no convention covers (an MCP tool, say). A caller deciding whether
/// to allow a call treats `None` as "cannot tell", never as harmless.
#[must_use]
pub fn known_capability_for_tool_name(name: &str) -> Option<ToolCapability> {
    let capability = match name {
        n if n.starts_with("delete_") => ToolCapability::DeleteFile,
        n if n.starts_with("write_") || n.starts_with("edit_") => ToolCapability::WriteFile,
        n if n.starts_with("read_")
            || n.starts_with("find_")
            || n.starts_with("search_")
            || n.starts_with("grep")
            || n.starts_with("list_")
            || n.starts_with("buf_")
            || n.starts_with("context_") =>
        {
            ToolCapability::ReadFile
        }
        n if n.starts_with("shell") || n.starts_with("run_terminal") || n.starts_with("gdb") => {
            ToolCapability::ExecuteShell
        }
        n if n.starts_with("web_") || n.starts_with("fetch") => ToolCapability::NetworkAccess,
        n if n.starts_with("git_") => ToolCapability::GitOperation,
        _ => return None,
    };
    Some(capability)
}

/// Validates that every effect produced by a dispatch is permitted in the
/// current state.
///
/// Runs on **every** dispatch before any effect is executed. Two failure modes,
/// both fatal to the batch:
///
/// * [`MachineError::ForbiddenToolCall`] - the capability is not in the
///   state's allow-set.
/// * [`MachineError::HumanApprovalRequired`] - the policy asks a person to
///   approve the call and nobody has (see
///   [`Context::is_call_approved`](crate::context::Context::is_call_approved)).
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

        if !policy.allows(state, cap) {
            return Err(MachineError::ForbiddenToolCall {
                state: PermissionPolicy::state_label(state),
                capability: cap,
            });
        }

        if policy.requires_approval(cap) && !approved(ctx, effect) {
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

    /// Two policies that disagree in every way the builder can express:
    /// per-state against global allowances, and different approval sets.
    fn disagreeing_policies() -> (PermissionPolicy, PermissionPolicy) {
        let a = PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile, ToolCapability::NetworkAccess])
            .allow_in(
                S::Executing,
                [ToolCapability::ExecuteShell, ToolCapability::WriteFile],
            )
            .require_approval([ToolCapability::WriteFile])
            .build();
        let b = PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile, ToolCapability::WriteFile])
            .allow_in(S::Executing, [ToolCapability::NetworkAccess])
            .allow_in(S::Reading, [ToolCapability::GitOperation])
            .require_approval([ToolCapability::NetworkAccess])
            .build();
        (a, b)
    }

    #[test]
    fn an_intersection_allows_only_what_both_sides_allow() {
        let (a, b) = disagreeing_policies();
        for merged in [a.intersect(&b), b.intersect(&a)] {
            for cap in ToolCapability::ALL {
                for state in [S::Reading, S::Executing] {
                    assert_eq!(
                        merged.allows(&state, cap),
                        a.allows(&state, cap) && b.allows(&state, cap),
                        "{cap:?} in {state:?}"
                    );
                }
                assert_eq!(
                    merged.requires_approval(cap),
                    a.requires_approval(cap) || b.requires_approval(cap),
                    "approval for {cap:?}"
                );
            }
        }
        let merged = a.intersect(&b);
        assert!(merged.allows(&S::Executing, ToolCapability::WriteFile));
        assert!(merged.allows(&S::Executing, ToolCapability::NetworkAccess));
        assert!(!merged.allows(&S::Reading, ToolCapability::NetworkAccess));
        assert!(!merged.allows(&S::Executing, ToolCapability::ExecuteShell));
    }

    #[test]
    fn a_ceiling_carries_one_state_authority_to_any_state() {
        let (a, _) = disagreeing_policies();
        let ceiling = a.ceiling_in(&S::Executing);
        for cap in ToolCapability::ALL {
            assert_eq!(
                ceiling.allows(&S::Reading, cap),
                a.allows(&S::Executing, cap)
            );
            assert_eq!(
                ceiling.allows_in_every_state(cap),
                a.allows(&S::Executing, cap)
            );
            assert_eq!(ceiling.requires_approval(cap), a.requires_approval(cap));
        }
        let everywhere = a.ceiling_in_every_state();
        assert!(everywhere.allows_in_every_state(ToolCapability::ReadFile));
        assert!(!everywhere.allows(&S::Executing, ToolCapability::ExecuteShell));
    }

    #[test]
    fn an_unknown_tool_name_has_no_known_capability() {
        assert_eq!(known_capability_for_tool_name("github-create_issue"), None);
        assert_eq!(
            known_capability_for_tool_name("shell"),
            Some(ToolCapability::ExecuteShell)
        );
        assert_eq!(
            capability_for_tool_name("github-create_issue"),
            ToolCapability::NetworkAccess
        );
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

    /// Auto approval: what the state allows runs, shell and delete included,
    /// without anyone being asked.
    #[test]
    fn nothing_needs_approval_unless_the_policy_asks_for_it() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Executing, ToolCapability::ALL)
            .build();
        for cap in ToolCapability::ALL {
            assert!(!policy.requires_approval(cap), "{cap:?}");
        }
        let ctx = Context::new();
        assert!(matches!(
            classify(&policy, &S::Executing, &ctx, &shell_tool()),
            EffectDisposition::Allowed
        ));
        assert!(
            validate_effects_are_allowed(&policy, &S::Executing, &[shell_tool()], &ctx).is_ok()
        );
    }

    /// Manual approval: every capability that is not read-only is asked
    /// about; the ceiling is untouched.
    #[test]
    fn manual_approval_asks_about_everything_that_is_not_read_only() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Executing, [ToolCapability::ReadFile])
            .build()
            .with_manual_approval();
        for cap in ToolCapability::ALL {
            assert_eq!(
                policy.requires_approval(cap),
                !cap.is_read_only(),
                "{cap:?}"
            );
        }
        let read_only: Vec<_> = ToolCapability::ALL
            .into_iter()
            .filter(|cap| cap.is_read_only())
            .collect();
        assert_eq!(
            read_only,
            [
                ToolCapability::ReadFile,
                ToolCapability::RunVerifier,
                ToolCapability::SpawnChild
            ]
        );
        assert!(!policy.allows(&S::Executing, ToolCapability::WriteFile));
        let ctx = Context::new();
        assert!(matches!(
            classify(&policy, &S::Executing, &ctx, &shell_tool()),
            EffectDisposition::Forbidden(_)
        ));
    }

    /// An approval is for the one call it was given for: the next call of
    /// the same capability is asked about again.
    #[test]
    fn an_approval_lets_only_the_call_it_was_given_for_run() {
        let policy = PermissionPolicy::builder()
            .allow_in(S::Executing, [ToolCapability::ExecuteShell])
            .build()
            .with_manual_approval();
        let mut ctx = Context::new();
        let approved = shell_tool();
        let next = shell_tool();
        let Effect::CallTool { call_id, .. } = &approved else {
            unreachable!()
        };
        assert!(matches!(
            classify(&policy, &S::Executing, &ctx, &approved),
            EffectDisposition::NeedsApproval(ToolCapability::ExecuteShell)
        ));
        let err = validate_effects_are_allowed(
            &policy,
            &S::Executing,
            std::slice::from_ref(&approved),
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, MachineError::HumanApprovalRequired { .. }));

        let approval_id = crate::ids::ApprovalId::from_uuid(call_id.as_uuid());
        ctx.set_pending_approval(crate::context::PendingApproval {
            approval_id,
            capability: ToolCapability::ExecuteShell,
            description: "run it".into(),
            call_id: Some(*call_id),
        });
        assert_eq!(ctx.approve(approval_id), Some(ToolCapability::ExecuteShell));

        assert!(matches!(
            classify(&policy, &S::Executing, &ctx, &approved),
            EffectDisposition::Allowed
        ));
        assert!(validate_effects_are_allowed(&policy, &S::Executing, &[approved], &ctx).is_ok());
        assert!(matches!(
            classify(&policy, &S::Executing, &ctx, &next),
            EffectDisposition::NeedsApproval(ToolCapability::ExecuteShell)
        ));
    }
}
