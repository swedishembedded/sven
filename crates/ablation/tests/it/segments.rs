// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The tokens of a task that ran in several processes: counted once.

use sven_ablation::record::task_ledger;
use sven_vocab::LedgerSummary;

fn summary(requests: u64, total: u64) -> LedgerSummary {
    LedgerSummary {
        requests,
        estimated_total: total,
        ..LedgerSummary::default()
    }
}

#[test]
fn a_restart_that_reloads_history_reports_the_whole_task_so_only_the_last_counts() {
    // Each process made 4, 4 and 5 requests; each saved trajectory holds
    // everything before it as well.
    let summaries = [summary(4, 20), summary(8, 60), summary(13, 120)];
    let counted = task_ledger(&summaries, &[4, 4, 5]).unwrap();
    assert_eq!(counted.len(), 1);
    assert_eq!(counted[0].estimated_total, 120);
}

#[test]
fn separate_trajectories_that_do_not_carry_history_are_summed() {
    let summaries = [summary(4, 20), summary(4, 40)];
    let counted = task_ledger(&summaries, &[4, 4]).unwrap();
    assert_eq!(counted.len(), 2);
}

#[test]
fn a_single_process_is_itself() {
    let counted = task_ledger(&[summary(3, 9)], &[3]).unwrap();
    assert_eq!(counted[0].estimated_total, 9);
}

#[test]
fn a_ledger_that_matches_neither_reading_is_an_error_and_not_a_guess() {
    let summaries = [summary(4, 20), summary(7, 60)];
    let error = task_ledger(&summaries, &[4, 4]).unwrap_err();
    assert!(error.contains("segment 1"), "{error}");
}
