// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a child agent run may do, and for how long.
//!
//! Every run a parent starts - an in-process child kernel, an agent in a
//! subprocess - runs under a [`ChildRunContract`]: the capabilities it may
//! exercise, the budgets it may spend and the instant it must stop by. A child
//! inherits its parent's terms and can only narrow them: the contract a child
//! runs under is always [`ChildRunContract::narrow`] of what its parent holds
//! and whatever further limits the spawner sets, and narrowing never allows
//! more than either side.
//!
//! The contract is plain data. Enforcing it is the job of whoever starts the
//! run: `sven-kernel` gives a child runtime the contract's policy and cancels
//! it at the deadline, and a subprocess spawner answers the child's
//! permission requests from it.
//!
//! Swedish Embedded AB implements delegation and capability-bounding for
//! agent runtimes for its clients. If your team needs expertise in
//! permission models for autonomous agents then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use crate::permissions::{PermissionPolicy, ToolCapability};

/// The capability set and budgets a child agent run inherits from its parent.
///
/// A child starts with a fresh context, so it holds none of its parent's
/// granted approvals: a capability the policy marks approval-required needs a
/// fresh approval in the child, from whatever approver the spawner provides.
#[derive(Clone, Debug, Default)]
pub struct ChildRunContract {
    /// What the child may do. Applied to the child's own machine, whose
    /// states the parent does not know, so it normally allows the same set in
    /// every state (see [`PermissionPolicy::ceiling_in`]).
    pub policy: PermissionPolicy,
    /// Most tool-call rounds a turn of the child may take, if limited.
    pub max_tool_rounds: Option<u32>,
    /// Most output tokens one model response of the child may produce, if
    /// limited.
    pub max_output_tokens: Option<u32>,
    /// When the child's run must be over, if bounded.
    pub deadline: Option<Instant>,
}

impl ChildRunContract {
    /// A contract that allows what `policy` allows, with no budgets.
    #[must_use]
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// The contract a parent in `state` under `policy` hands down: exactly
    /// the capabilities that state holds, in every state of the child.
    #[must_use]
    pub fn inherit<S: Debug>(policy: &PermissionPolicy, state: &S) -> Self {
        Self::new(policy.ceiling_in(state))
    }

    /// Limits the tool-call rounds of the child's turns.
    #[must_use]
    pub fn with_max_tool_rounds(mut self, rounds: u32) -> Self {
        self.max_tool_rounds = Some(rounds);
        self
    }

    /// Limits the output tokens of each of the child's model responses.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Requires the child's run to be over `budget` after `now`. A budget so
    /// large that the instant cannot be represented sets no deadline.
    #[must_use]
    pub fn with_deadline_after(self, now: Instant, budget: Duration) -> Self {
        match now.checked_add(budget) {
            Some(deadline) => self.with_deadline(deadline),
            None => self,
        }
    }

    /// Requires the child's run to be over by `deadline`.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// The contract that satisfies both `self` and `other`: the intersection
    /// of their policies, the smaller of each budget and the earlier
    /// deadline. It never allows more, for longer or at greater cost than
    /// either side.
    #[must_use]
    pub fn narrow(&self, other: &Self) -> Self {
        Self {
            policy: self.policy.intersect(&other.policy),
            max_tool_rounds: tighter(self.max_tool_rounds, other.max_tool_rounds),
            max_output_tokens: tighter(self.max_output_tokens, other.max_output_tokens),
            deadline: tighter(self.deadline, other.deadline),
        }
    }

    /// `true` if the child may use `capability` in any state without anyone
    /// being asked first.
    #[must_use]
    pub fn allows_without_asking(&self, capability: ToolCapability) -> bool {
        self.policy.allows_in_every_state(capability) && !self.policy.requires_approval(capability)
    }

    /// Time left before the deadline at `now`: `None` when the run is not
    /// time-bounded, zero once the deadline has passed.
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
}

/// The tighter of two optional limits, where `None` means unlimited.
fn tighter<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    enum Parent {
        Planning,
        Executing,
    }

    fn parent_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .allow_in(
                Parent::Executing,
                [ToolCapability::WriteFile, ToolCapability::ExecuteShell],
            )
            .build()
    }

    fn fixed_child_terms() -> ChildRunContract {
        ChildRunContract::new(
            PermissionPolicy::builder()
                .allow_globally([
                    ToolCapability::ReadFile,
                    ToolCapability::WriteFile,
                    ToolCapability::ExecuteShell,
                    ToolCapability::NetworkAccess,
                ])
                .build(),
        )
        .with_max_tool_rounds(40)
    }

    #[test]
    fn a_child_holds_only_what_its_parent_holds_in_the_spawning_state() {
        let child = ChildRunContract::inherit(&parent_policy(), &Parent::Planning)
            .narrow(&fixed_child_terms());
        assert!(child.policy.allows_in_every_state(ToolCapability::ReadFile));
        for denied in [
            ToolCapability::WriteFile,
            ToolCapability::ExecuteShell,
            ToolCapability::NetworkAccess,
        ] {
            assert!(!child.policy.allows_in_every_state(denied), "{denied:?}");
        }

        let child = ChildRunContract::inherit(&parent_policy(), &Parent::Executing)
            .narrow(&fixed_child_terms());
        assert!(child
            .policy
            .allows_in_every_state(ToolCapability::WriteFile));
        assert!(
            !child
                .policy
                .allows_in_every_state(ToolCapability::NetworkAccess),
            "the parent never held network, so the child's own terms cannot add it"
        );
    }

    #[test]
    fn narrowing_takes_the_smaller_budget_and_the_earlier_deadline() {
        let now = Instant::now();
        let parent = ChildRunContract::new(parent_policy())
            .with_max_tool_rounds(100)
            .with_deadline(now + Duration::from_secs(60));
        let terms = ChildRunContract::new(parent_policy())
            .with_max_tool_rounds(40)
            .with_max_output_tokens(4096)
            .with_deadline(now + Duration::from_secs(600));
        for child in [parent.narrow(&terms), terms.narrow(&parent)] {
            assert_eq!(child.max_tool_rounds, Some(40));
            assert_eq!(child.max_output_tokens, Some(4096));
            assert_eq!(child.deadline, Some(now + Duration::from_secs(60)));
        }
        let unbounded = ChildRunContract::new(parent_policy());
        assert_eq!(unbounded.narrow(&unbounded).deadline, None);
    }

    #[test]
    fn a_capability_that_needs_approval_is_not_allowed_without_asking() {
        let contract = ChildRunContract::new(
            PermissionPolicy::builder()
                .allow_globally([
                    ToolCapability::ReadFile,
                    ToolCapability::WriteFile,
                    ToolCapability::ExecuteShell,
                ])
                .require_approval([ToolCapability::WriteFile])
                .build(),
        );
        assert!(contract.allows_without_asking(ToolCapability::ReadFile));
        assert!(!contract.allows_without_asking(ToolCapability::WriteFile));
        assert!(
            !contract.allows_without_asking(ToolCapability::ExecuteShell),
            "shell is inherently dangerous"
        );
        assert!(!contract.allows_without_asking(ToolCapability::NetworkAccess));
    }

    #[test]
    fn a_budget_beyond_what_an_instant_can_hold_means_no_deadline() {
        let now = Instant::now();
        let contract = ChildRunContract::default().with_deadline_after(now, Duration::MAX);
        assert_eq!(contract.deadline, None);
        let contract = ChildRunContract::default().with_deadline_after(now, Duration::from_secs(5));
        assert_eq!(contract.deadline, Some(now + Duration::from_secs(5)));
    }

    #[test]
    fn remaining_time_is_zero_once_the_deadline_has_passed() {
        let now = Instant::now();
        let contract = ChildRunContract::default().with_deadline(now + Duration::from_secs(5));
        assert_eq!(contract.remaining(now), Some(Duration::from_secs(5)));
        assert_eq!(
            contract.remaining(now + Duration::from_secs(9)),
            Some(Duration::ZERO)
        );
        assert_eq!(ChildRunContract::default().remaining(now), None);
    }
}
