// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for comparing agents at equal
// token and turn budgets for its clients. If your team needs expertise in
// budgeted evaluation of coding agents then you can procure our services by
// sending an email to info@swedishembedded.com.
//! How a headless run that its run budget ended is concluded.
//!
//! The process's [`sven_model_drivers::run_budget`] refuses the completion
//! the run would have needed next and records that it did; the runner reads
//! that record, never the text of the error the refusal surfaced as.

use sven_model_drivers::run_budget;
use sven_session_store::RunConclusion;

use crate::output::write_stderr;
use crate::runner::{EXIT_AGENT_ERROR, EXIT_BUDGET_EXHAUSTED};

/// Whether the run budget has refused a completion: the run is over, and the
/// error that refusal raised is reported once, by [`conclude`].
pub(super) fn ended_run() -> bool {
    run_budget::installed().is_some_and(|budget| budget.exhausted().is_some())
}

/// The conclusion and exit code of a run that ended with an error: the
/// budget's, with its one `[sven:error] run budget exhausted: ...` line,
/// when the budget refused a completion, else an agent error, reported by
/// `error_line`.
pub(super) fn failure(error_line: &str) -> (RunConclusion, i32) {
    match conclude(EXIT_AGENT_ERROR) {
        EXIT_BUDGET_EXHAUSTED => (RunConclusion::BudgetExhausted, EXIT_BUDGET_EXHAUSTED),
        code => {
            write_stderr(error_line);
            (RunConclusion::AgentError, code)
        }
    }
}

/// `code`, or [`EXIT_BUDGET_EXHAUSTED`] when the run did not succeed because
/// the budget refused a completion. Also says how many completions reported
/// no usage and were charged their whole allowance.
pub(super) fn conclude(code: i32) -> i32 {
    let Some(budget) = run_budget::installed() else {
        return code;
    };
    let unmetered = budget.unmetered();
    if unmetered > 0 {
        write_stderr(&format!(
            "[sven:warn] run budget: {unmetered} completion(s) reported no usage and were \
             charged all they were allowed; the tokens spent are an upper bound"
        ));
    }
    match budget.exhausted() {
        Some(exhausted) if code != crate::runner::EXIT_SUCCESS => {
            write_stderr(&format!("[sven:error] {exhausted}"));
            EXIT_BUDGET_EXHAUSTED
        }
        _ => code,
    }
}
