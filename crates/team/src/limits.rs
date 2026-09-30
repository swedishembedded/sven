// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a team member's runs may use: which tools, how many tool rounds per
//! task, and how many of the team's tokens.
//!
//! The terms live in the team config the lead writes and every member reads,
//! so a member process learns them from the same file it takes its tasks
//! from, before each task.
//!
//! Swedish Embedded AB implements bounded multi-agent teams for its clients.
//! If your team needs expertise in budgeting and constraining autonomous
//! agents then you can procure our services by sending an email to
//! info@swedishembedded.com.

use crate::config::{MemberStatus, TeamConfig};

/// How many of the team's tokens a member's next run may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenAllowance {
    /// The team has no token budget.
    Unlimited,
    /// This many tokens are set aside for the run.
    Remaining(u64),
    /// The team's budget is spent: no further run may start.
    Exhausted,
}

impl TeamConfig {
    /// Sets aside one run's share of what is left of the token budget: the
    /// remainder split evenly between the members working now, so members
    /// running at once never together spend more than the budget. The share
    /// counts as used until [`Self::settle_tokens`] replaces it with what the
    /// run actually used.
    pub fn reserve_tokens(&mut self) -> TokenAllowance {
        if self.token_budget == 0 {
            return TokenAllowance::Unlimited;
        }
        let left = self.token_budget.saturating_sub(self.tokens_used);
        if left == 0 {
            return TokenAllowance::Exhausted;
        }
        let working = self
            .members
            .iter()
            .filter(|m| m.status == MemberStatus::Active)
            .count()
            .max(1) as u64;
        let share = (left / working).max(1);
        self.tokens_used += share;
        TokenAllowance::Remaining(share)
    }

    /// Replaces a run's reservation with the `used` tokens it actually spent.
    pub fn settle_tokens(&mut self, reserved: TokenAllowance, used: u64) {
        let reserved = match reserved {
            TokenAllowance::Remaining(share) => share,
            TokenAllowance::Unlimited | TokenAllowance::Exhausted => 0,
        };
        self.tokens_used = self
            .tokens_used
            .saturating_sub(reserved)
            .saturating_add(used);
    }
}

/// The terms a member's runs are held to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberLimits {
    /// Tools the member's runs may not use, by name.
    pub deny_tools: Vec<String>,
    /// Most tool rounds a task run may take, if limited.
    pub max_tool_rounds: Option<u32>,
}

impl MemberLimits {
    /// The terms `team` sets for the member with `peer_id`. A member the
    /// config does not list is held to the team-wide terms alone.
    #[must_use]
    pub fn for_member(team: &TeamConfig, peer_id: &str) -> Self {
        let deny_tools = team
            .find_member(peer_id)
            .map(|m| m.deny_tools.clone())
            .unwrap_or_default();
        Self {
            deny_tools,
            max_tool_rounds: (team.max_iterations > 0).then_some(team.max_iterations),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team() -> TeamConfig {
        let mut team = TeamConfig::new("t", "lead", "lead");
        let mut reviewer = team.members[0].clone();
        reviewer.peer_id = "tm-reviewer".into();
        reviewer.deny_tools = vec!["write_file".into(), "edit_file".into()];
        team.members.push(reviewer);
        team
    }

    #[test]
    fn a_member_is_held_to_its_own_deny_list_and_the_team_limits() {
        let mut team = team();
        team.max_iterations = 12;
        let limits = MemberLimits::for_member(&team, "tm-reviewer");
        assert_eq!(limits.deny_tools, vec!["write_file", "edit_file"]);
        assert_eq!(limits.max_tool_rounds, Some(12));

        let stranger = MemberLimits::for_member(&team, "tm-unknown");
        assert!(stranger.deny_tools.is_empty());
        team.max_iterations = 0;
        assert_eq!(
            MemberLimits::for_member(&team, "tm-reviewer").max_tool_rounds,
            None
        );
    }

    fn active(team: &mut TeamConfig, n: usize) {
        for m in team.members.iter_mut() {
            m.status = MemberStatus::Unknown;
        }
        for i in 0..n {
            let mut member = team.members[0].clone();
            member.peer_id = format!("tm-{i}");
            member.status = MemberStatus::Active;
            team.members.push(member);
        }
    }

    #[test]
    fn members_running_at_once_never_reserve_more_than_the_budget() {
        let mut team = team();
        team.token_budget = 1_000;
        team.tokens_used = 400;
        active(&mut team, 3);
        let shares: Vec<TokenAllowance> = (0..3).map(|_| team.reserve_tokens()).collect();
        let total: u64 = shares
            .iter()
            .map(|a| match a {
                TokenAllowance::Remaining(n) => *n,
                other => panic!("{other:?}"),
            })
            .sum();
        assert!(total <= 600, "reserved {total} of 600");
        assert!(team.tokens_used <= 1_000);

        // Settling replaces the reservation with what was used.
        let before = team.tokens_used;
        team.settle_tokens(shares[0], 50);
        let TokenAllowance::Remaining(first) = shares[0] else {
            unreachable!()
        };
        assert_eq!(team.tokens_used, before - first + 50);
    }

    #[test]
    fn a_spent_budget_refuses_and_no_budget_is_unlimited() {
        let mut team = team();
        assert_eq!(team.reserve_tokens(), TokenAllowance::Unlimited);
        team.token_budget = 1_000;
        team.tokens_used = 1_000;
        assert_eq!(team.reserve_tokens(), TokenAllowance::Exhausted);
        team.tokens_used = 5_000;
        assert_eq!(team.reserve_tokens(), TokenAllowance::Exhausted);
        team.settle_tokens(TokenAllowance::Exhausted, 0);
        assert_eq!(team.tokens_used, 5_000);
    }

    #[test]
    fn a_run_without_a_budget_is_still_charged() {
        let mut team = team();
        team.settle_tokens(TokenAllowance::Unlimited, 70);
        assert_eq!(team.tokens_used, 70);
    }
}
