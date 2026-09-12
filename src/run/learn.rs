// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven learn` - the synchronous, scriptable entry point to the pending-facts
//! drain.
//!
//! A one-shot, non-interactive run - `sven --headless "learn what you can
//! from spec.md"` inside a shell script - exits the moment the prompt is
//! answered. A fact still sitting in the ledger at that instant is a fact
//! nothing will ever come back for, and the script has no way to wait for one
//! except to guess at a sleep. This module is a blocking entry point that
//! drains everything pending and returns only when every fact has a real
//! verdict.
//!
//! [`PendingFactsDrain`]'s exactly-once settlement rests on a cursor file with
//! a single owner: two `sven learn flush` invocations racing the same cursor
//! could otherwise double-submit a batch or lose an outcome. `flush` therefore
//! claims [`sven_memory::claim_sole_drain`]'s exclusive advisory lock before
//! it touches the cursor at all, and a flush that loses that race fails
//! cleanly rather than proceeding unguarded.
//!
//! Swedish Embedded AB implements solutions for scriptable, auditable
//! continuous-learning pipelines for its clients. If your team needs expertise
//! in wiring an agent's knowledge capture into CI then you can procure our
//! services by sending an email to info@swedishembedded.com.

use sven_config::Config;
use sven_memory::{
    claim_sole_drain, FactOutcome, FactReport, GateNumbers, PendingFactsDrain, PendingFactsLedger,
};

use crate::cli::LearnCommands;

pub(crate) async fn run_learn_command(cmd: &LearnCommands, config: &Config) -> anyhow::Result<()> {
    match cmd {
        LearnCommands::Flush { json } => flush(config, *json).await,
        LearnCommands::Doctor => doctor(config),
    }
}

/// Runs every preflight check and prints one report naming every problem at
/// once. Exits non-zero only if at least one check actually failed - a
/// warning (e.g. `submitter: "none"`, or too few facts so far) is not a
/// reason to fail a script that is only checking readiness.
fn doctor(config: &Config) -> anyhow::Result<()> {
    let report = sven_memory::diagnose(config);
    println!("{}", sven_memory::format_report(&report));
    if report.is_healthy() {
        Ok(())
    } else {
        anyhow::bail!("one or more checks failed; see [fail] lines above")
    }
}

/// Drains every pending fact and blocks until each has an outcome.
///
/// Deliberately not gated on `tools.memory.learning.submit_facts`. That flag
/// decides whether facts leave *unattended*, in the background, without anyone
/// asking; typing `sven learn flush` is the asking. The submitter selection
/// still applies, so a machine that configured `submitter: none` gets a clear
/// refusal rather than a silent no-op.
///
/// Takes the ledger's single-drain lock before touching the cursor: two
/// `sven learn flush` invocations racing the same cursor file could otherwise
/// double-submit or lose facts, exactly what [`PendingFactsDrain`]'s
/// exactly-once protocol exists to prevent. A flush that loses the race fails
/// cleanly instead of touching the cursor.
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
    let cursor_path = PendingFactsDrain::default_cursor_path(&ledger);

    // Held for the life of this flush: dropped at the end of the function,
    // releasing the lock for the next invocation.
    let Some(_sole_drain_lock) = claim_sole_drain(&cursor_path)? else {
        anyhow::bail!(
            "another `sven learn flush` is already draining {}; wait for it to finish and try again.",
            cursor_path.display()
        );
    };

    let drain = PendingFactsDrain::new(
        ledger.clone(),
        cursor_path,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use sven_memory::{
        claim_sole_drain, FactOutcome, FactReport, FactSubmitter, GateNumbers, PendingFactRecord,
        PendingFactsDrain, PendingFactsLedger,
    };
    use sven_vocab::provenance::{FactId, FactSource};

    #[derive(Default)]
    struct CountingSubmitter {
        submitted: Mutex<Vec<FactId>>,
    }

    impl CountingSubmitter {
        fn submitted(&self) -> Vec<FactId> {
            self.submitted.lock().expect("submitted lock").clone()
        }
    }

    #[async_trait]
    impl FactSubmitter for CountingSubmitter {
        async fn submit(&self, batch: &[PendingFactRecord]) -> anyhow::Result<Vec<FactReport>> {
            self.submitted
                .lock()
                .expect("submitted lock")
                .extend(batch.iter().map(|f| f.id.clone()));
            Ok(batch
                .iter()
                .map(|f| FactReport {
                    id: f.id.clone(),
                    outcome: FactOutcome::Promoted {
                        numbers: GateNumbers::default(),
                    },
                })
                .collect())
        }

        async fn outcomes_for(&self, _facts: &[FactId]) -> anyhow::Result<Vec<FactReport>> {
            Ok(Vec::new())
        }
    }

    /// `flush` claims the ledger's single-drain lock before it ever
    /// constructs a [`PendingFactsDrain`] or touches the cursor - reproduced
    /// here against a temporary ledger, since `flush` itself always resolves
    /// `PendingFactsLedger::at_default_path()`. A concurrent second flush
    /// over the same cursor must fail to claim the lock, so it never runs
    /// `drain_all` at all, and the fact settles exactly once - not twice, not
    /// never.
    #[tokio::test]
    async fn a_concurrent_flush_over_the_same_cursor_fails_cleanly_without_double_submitting() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
        ledger
            .record_fact(&PendingFactRecord {
                id: FactId::new("f-1"),
                fact: "Fact f-1.".to_string(),
                probe: None,
                source: FactSource::UserStated,
                recorded_at: 1,
            })
            .expect("record fact");
        let cursor_path = PendingFactsDrain::default_cursor_path(&ledger);

        // The first flush claims the lock, exactly as `flush` does.
        let first_lock = claim_sole_drain(&cursor_path)
            .expect("lock io")
            .expect("the first flush claims the lock");

        // A second, concurrent flush must fail to claim it - and therefore
        // never construct a drain or touch the cursor at all.
        assert!(
            claim_sole_drain(&cursor_path).expect("lock io").is_none(),
            "a concurrent flush must not also claim the lock"
        );

        // The flush holding the lock proceeds exactly as `flush` does.
        let submitter = CountingSubmitter::default();
        let drain = PendingFactsDrain::new(ledger.clone(), &cursor_path, 8);
        let reports = drain.drain_all(&submitter).await.expect("drain");
        assert_eq!(reports.len(), 1, "the fact settles exactly once");
        assert_eq!(submitter.submitted(), vec![FactId::new("f-1")]);

        drop(first_lock);

        // Once released, a subsequent flush may claim the lock and finds
        // nothing left to do - the cursor was never corrupted or replayed.
        let second_lock = claim_sole_drain(&cursor_path)
            .expect("lock io")
            .expect("the lock is claimable again once released");
        let second_reports = PendingFactsDrain::new(ledger, &cursor_path, 8)
            .drain_all(&submitter)
            .await
            .expect("idle drain");
        assert!(
            second_reports.is_empty(),
            "a settled ledger has nothing left to drain"
        );
        assert_eq!(
            submitter.submitted().len(),
            1,
            "the fact was never submitted twice"
        );
        drop(second_lock);
    }
}
