// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A sub-agent's share of its parent's run budget: what the child reports
//! spending of the budget it was handed is charged to the parent's.

use sven_model_drivers::run_budget::Spent;
use tracing::warn;

use super::report::TurnTokens;

/// What a child's turn took of its run budget, from its response metadata.
pub(super) fn budget_spent(meta: Option<&agent_client_protocol::Meta>) -> Option<Spent> {
    let value = meta?.get(sven_model_drivers::run_budget::SPENT_META_KEY)?;
    serde_json::from_value(value.clone())
        .map_err(|e| warn!("task: the child's run-budget report is unreadable: {e}"))
        .ok()
}

/// Charges a child's turn to this process's run budget, if one is in force:
/// what the child reported spending of the budget it was handed, or, from a
/// child that reported none, its output tokens (its completions uncounted).
pub(super) fn charge_run_budget(tokens: &TurnTokens) {
    let Some(budget) = sven_model_drivers::run_budget::installed() else {
        return;
    };
    budget.charge(tokens.budget.unwrap_or_else(|| {
        warn!(
            "task: the child reported no run-budget spending; charging its {} output tokens \
             and no completions",
            tokens.output
        );
        Spent {
            completion_tokens: tokens.output,
            turns: 0,
        }
    }));
}
