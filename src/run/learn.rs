// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven learn` - the synchronous half of the pending-facts drain.
//!
//! The drain's normal home is a background `tokio` task in `sven-frontend`,
//! started when a session opens and ticking on an interval. That is right for
//! an interactive TUI and useless for the case this module exists for: a
//! one-shot, non-interactive run - `sven --headless "learn what you can from
//! spec.md"` inside a shell script - which exits the moment the prompt is
//! answered. A fact still sitting in the ledger at that instant is a fact
//! nothing will ever come back for, and the script has no way to wait for one
//! except to guess at a sleep.
//!
//! So this is additive, not a replacement: the background task is untouched
//! and the interactive path behaves exactly as it did. What is new is a
//! blocking entry point that drains everything pending and returns only when
//! every fact has a real verdict.
//!
//! Swedish Embedded AB implements solutions for scriptable, auditable
//! continuous-learning pipelines for its clients. If your team needs expertise
//! in wiring an agent's knowledge capture into CI then you can procure our
//! services by sending an email to info@swedishembedded.com.

use sven_config::Config;
use sven_memory::{FactOutcome, FactReport, GateNumbers, PendingFactsDrain, PendingFactsLedger};

use crate::cli::LearnCommands;

pub(crate) async fn run_learn_command(cmd: &LearnCommands, config: &Config) -> anyhow::Result<()> {
    match cmd {
        LearnCommands::Flush { json } => flush(config, *json).await,
        LearnCommands::ExportTrajectories { runs, min_reward, out } => export_trajectories(runs, *min_reward, out),
    }
}

/// Blocking on purpose: this is a small, local, file-to-file operation - no
/// submitter, no subprocess, nothing to await.
fn export_trajectories(runs: &std::path::Path, min_reward: f64, out: &std::path::Path) -> anyhow::Result<()> {
    let summary = sven_memory::export_trajectories(runs, min_reward, out)?;
    println!(
        "scanned {}, exported {}, no reward {}, below threshold {} -> {}",
        summary.scanned,
        summary.exported,
        summary.no_reward,
        summary.below_threshold,
        out.display()
    );
    Ok(())
}

/// Drains every pending fact and blocks until each has an outcome.
///
/// Deliberately not gated on `tools.memory.learning.submit_facts`. That flag
/// decides whether facts leave *unattended*, in the background, without anyone
/// asking; typing `sven learn flush` is the asking. The submitter selection
/// still applies, so a machine that configured `submitter: none` gets a clear
/// refusal rather than a silent no-op.
async fn flush(config: &Config, json: bool) -> anyhow::Result<()> {
    let Some(submitter) = sven_memory::submitter_from_config(config)? else {
        anyhow::bail!(
            "tools.memory.learning.submitter is \"none\": there is nothing to flush facts to. \
             Set it to \"local\" to run brain's document study on this machine."
        );
    };

    // The same ledger `RuntimeBuilder` hands `assimilate_fact`/
    // `ingest_document`, by construction rather than by a second path
    // expression: a drain reading a different file than the writer writes
    // would fail silently and forever.
    let ledger = PendingFactsLedger::at_default_path();
    let drain = PendingFactsDrain::new(
        ledger.clone(),
        PendingFactsDrain::default_cursor_path(&ledger),
        config.tools.memory.learning.batch_size,
    );

    let reports = drain.drain_all(submitter.as_ref()).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_reports(&reports);
    }

    // A rejection is a real answer - the study ran and the model did not learn
    // the fact - and a script that failed its build over one would be wrong. A
    // `failed` fact is different: nothing was decided, and that is a fault the
    // script's operator should see.
    let failed = reports
        .iter()
        .filter(|r| matches!(r.outcome, FactOutcome::Failed { .. }))
        .count();
    if failed > 0 {
        anyhow::bail!("{failed} fact(s) could not be decided; see the reasons above");
    }
    Ok(())
}

fn print_reports(reports: &[FactReport]) {
    if reports.is_empty() {
        println!("Nothing pending: every recorded fact already has an outcome.");
        return;
    }
    for report in reports {
        match &report.outcome {
            FactOutcome::Promoted { numbers } => {
                println!(
                    "promoted  {}{}",
                    report.id.as_str(),
                    rates_suffix(numbers)
                );
            }
            FactOutcome::Rejected { reason, .. } => {
                println!("rejected  {}  {reason}", report.id.as_str());
            }
            FactOutcome::Failed { reason } => {
                println!("failed    {}  {reason}", report.id.as_str());
            }
        }
    }
    let promoted = reports
        .iter()
        .filter(|r| matches!(r.outcome, FactOutcome::Promoted { .. }))
        .count();
    println!("{promoted}/{} fact(s) promoted.", reports.len());
}

/// `" (probe pass rate: baseline X -> Y)"`, or nothing when the gate's report
/// carried no numbers for this promotion (e.g. a mock run).
fn rates_suffix(numbers: &GateNumbers) -> String {
    match (numbers.baseline_pass_rate, numbers.post_training_pass_rate) {
        (Some(base), Some(trained)) => format!(" (probe pass rate: baseline {base} -> {trained})"),
        (None, Some(trained)) => format!(" (probe pass rate after training: {trained})"),
        _ => String::new(),
    }
}
