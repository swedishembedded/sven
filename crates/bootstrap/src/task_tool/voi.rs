// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The value of delegating a subtask: is a sub-agent worth what it costs?
//!
//! A sub-agent pays for its own system prompt and tool definitions, reads the
//! brief, and hands a report back to the parent. Work done inline instead
//! stays in the parent's context and is sent again in every later request.
//! [`assess`] compares the two in estimated tokens, the way `rlcd::cost` weighs
//! an expected benefit against what coordinating it costs, and delegating is
//! worth it only when `expected_benefit > coordination_cost`:
//!
//! ```text
//! coordination_cost = child_overhead + brief + report (+ write_surcharge)
//! expected_benefit  = inline_work * (1 + parent_context / window) * (1 + replay_rounds)
//!                     + (independent_subgoals - 1) * parallel_credit
//! ```
//!
//! `inline_work` is measured when the subtask declares literal files (their
//! sizes) and a configured assumption otherwise. `child_overhead` and `report`
//! are the averages the session's children measured once `min_samples` have
//! finished, the configured values before. Every number but the ones measured
//! is the configuration's: nothing here is a constant of the model.
//!
//! The function is pure. It estimates; it decides nothing about permissions or
//! concurrency, and `delegation.policy: off` (the default) never calls it.

use sven_tool_api::PathScope;

use crate::config::DelegationConfig;

/// What the session has measured of its finished children, in tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Observed {
    /// Average input tokens of a child's turn: what it paid before and while
    /// working, the closest measure of its overhead.
    pub(super) child_overhead: Option<u64>,
    /// Average size of what children handed back.
    pub(super) report: Option<u64>,
}

/// What is known of the subtask about to be delegated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Subtask {
    /// Tokens of the child's first request.
    pub(super) brief_tokens: u64,
    /// Tokens of the work if done inline, when the subtask declares it.
    pub(super) inline_work_tokens: Option<u64>,
    /// Tokens already in the parent's context (or the best proxy for them).
    pub(super) parent_context_tokens: u64,
    /// Subgoals in flight at the same time, this one included.
    pub(super) independent_subgoals: u64,
    pub(super) writes: bool,
}

/// The estimate and what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Weighed {
    pub(super) expected_benefit: u64,
    pub(super) coordination_cost: u64,
    /// Whether overhead and report came from measured children.
    pub(super) measured: bool,
}

impl Weighed {
    /// Delegating is worth it.
    pub(super) fn worth_it(&self) -> bool {
        self.expected_benefit > self.coordination_cost
    }

    /// The estimate in words, for the result of a call.
    pub(super) fn explain(&self) -> String {
        format!(
            "a sub-agent costs about {} tokens (its own prompt, the brief and the report; {}) \
             and saves about {}",
            self.coordination_cost,
            if self.measured {
                "measured on this session's earlier sub-agents"
            } else {
                "configured, no sub-agent has been measured yet"
            },
            self.expected_benefit
        )
    }
}

/// Weighs delegating `subtask`.
pub(super) fn assess(config: &DelegationConfig, observed: &Observed, subtask: &Subtask) -> Weighed {
    let overhead = observed
        .child_overhead
        .unwrap_or(config.child_overhead_tokens);
    let report = observed.report.unwrap_or(config.report_tokens);
    let surcharge = if subtask.writes {
        config.write_surcharge_tokens
    } else {
        0
    };
    let coordination_cost = overhead
        .saturating_add(subtask.brief_tokens)
        .saturating_add(report)
        .saturating_add(surcharge);

    let work = u128::from(
        subtask
            .inline_work_tokens
            .unwrap_or(config.unknown_work_tokens),
    );
    let window = u128::from(config.context_window_tokens.max(1));
    let fuller = window + u128::from(subtask.parent_context_tokens);
    let rounds = u128::from(config.replay_rounds) + 1;
    let replayed = work.saturating_mul(fuller).saturating_mul(rounds) / window;
    let parallel = u128::from(subtask.independent_subgoals.saturating_sub(1))
        * u128::from(config.parallel_credit_tokens);
    Weighed {
        expected_benefit: u64::try_from(replayed.saturating_add(parallel)).unwrap_or(u64::MAX),
        coordination_cost,
        measured: observed.child_overhead.is_some() && observed.report.is_some(),
    }
}

/// The tokens of the work in the literal files `paths` name, from their sizes
/// (bytes / 4, the ledger's estimate). `None` when there are none, or one is
/// a glob or cannot be measured: nothing then says how big the work is.
pub(super) fn declared_work_tokens(paths: &[String], scope: &PathScope) -> Option<u64> {
    if paths.is_empty() {
        return None;
    }
    paths.iter().try_fold(0u64, |total, p| {
        if p.contains(['*', '?']) {
            return None;
        }
        let len = std::fs::metadata(scope.resolve(p).ok()?).ok()?.len();
        Some(total.saturating_add(len.div_ceil(4)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DelegationConfig {
        DelegationConfig::default()
    }

    fn subtask(work: Option<u64>) -> Subtask {
        Subtask {
            brief_tokens: 100,
            inline_work_tokens: work,
            parent_context_tokens: 0,
            independent_subgoals: 1,
            writes: false,
        }
    }

    #[test]
    fn a_trivial_subtask_is_not_worth_a_sub_agent() {
        let w = assess(&cfg(), &Observed::default(), &subtask(Some(50)));
        assert!(!w.worth_it(), "{w:?}");
        assert_eq!(w.coordination_cost, 4_000 + 100 + 400);
        assert_eq!(w.expected_benefit, 50 * 4);
    }

    #[test]
    fn reading_a_lot_is_worth_a_sub_agent() {
        assert!(assess(&cfg(), &Observed::default(), &subtask(Some(20_000))).worth_it());
    }

    #[test]
    fn an_undeclared_subtask_is_assumed_to_be_the_configured_size() {
        let w = assess(&cfg(), &Observed::default(), &subtask(None));
        assert_eq!(w.expected_benefit, 6_000 * 4);
        assert!(w.worth_it());
    }

    #[test]
    fn a_fuller_parent_and_more_parallel_subgoals_raise_the_benefit() {
        let base = assess(&cfg(), &Observed::default(), &subtask(Some(1_000)));
        let mut full = subtask(Some(1_000));
        full.parent_context_tokens = 128_000;
        assert_eq!(
            assess(&cfg(), &Observed::default(), &full).expected_benefit,
            base.expected_benefit * 2
        );
        let mut many = subtask(Some(1_000));
        many.independent_subgoals = 5;
        assert_eq!(
            assess(&cfg(), &Observed::default(), &many).expected_benefit,
            base.expected_benefit + 4 * 1_500
        );
    }

    #[test]
    fn writing_costs_more_and_measured_numbers_replace_the_configured_ones() {
        let read = assess(&cfg(), &Observed::default(), &subtask(Some(1_000)));
        let mut write = subtask(Some(1_000));
        write.writes = true;
        assert_eq!(
            assess(&cfg(), &Observed::default(), &write).coordination_cost,
            read.coordination_cost + 1_000
        );
        let seen = Observed {
            child_overhead: Some(900),
            report: Some(100),
        };
        let w = assess(&cfg(), &seen, &subtask(Some(1_000)));
        assert_eq!(w.coordination_cost, 900 + 100 + 100);
        assert!(w.measured && !read.measured);
        assert!(w.explain().contains("measured"));
        assert!(read.explain().contains("configured"));
    }

    #[test]
    fn extreme_numbers_do_not_overflow() {
        let huge = DelegationConfig {
            context_window_tokens: 1,
            replay_rounds: u64::MAX - 1,
            ..cfg()
        };
        let mut s = subtask(Some(u64::MAX));
        s.parent_context_tokens = u64::MAX;
        let w = assess(&huge, &Observed::default(), &s);
        assert_eq!(w.expected_benefit, u64::MAX);
    }

    #[test]
    fn declared_literal_files_are_measured_and_anything_else_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x".repeat(400)).unwrap();
        std::fs::write(dir.path().join("b.rs"), "y".repeat(40)).unwrap();
        let scope = PathScope::confined(dir.path()).unwrap();
        let p = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            declared_work_tokens(&p(&["a.rs", "b.rs"]), &scope),
            Some(110)
        );
        assert_eq!(declared_work_tokens(&p(&[]), &scope), None);
        assert_eq!(declared_work_tokens(&p(&["a.rs", "*.rs"]), &scope), None);
        assert_eq!(declared_work_tokens(&p(&["missing.rs"]), &scope), None);
        assert_eq!(declared_work_tokens(&p(&["../outside"]), &scope), None);
    }
}
