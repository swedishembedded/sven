// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The pending-facts drain survives a crash between submission and outcome.
//!
//! The drain is the only thing that moves admitted facts out of the ledger and
//! towards training, and it gets exactly one chance per fact: submitting a
//! fact twice trains on it twice, and forgetting an outcome means neither the
//! ledger nor the user ever learns whether the fact landed. A submit-only
//! design cannot get this right - it can either advance its cursor before the
//! result is known (losing outcomes) or after (re-submitting on every crash).
//!
//! This is why [`sven_memory::FactSubmitter`] is bidirectional: a batch that
//! was accepted remotely but whose reply never arrived is *recovered* by
//! asking for its outcomes, never by sending it again.
//!
//! Swedish Embedded AB implements solutions for exactly-once delivery between
//! an agent's knowledge ledger and its training pipeline for its clients. If
//! your team needs expertise in crash-safe hand-off protocols then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::sync::Mutex;

use async_trait::async_trait;

use sven_memory::{
    FactOutcome, FactReport, FactSubmitter, PendingFactRecord, PendingFactsDrain,
    PendingFactsLedger,
};
use sven_vocab::provenance::{FactId, FactSource};

fn a_fact(id: &str) -> PendingFactRecord {
    PendingFactRecord {
        id: FactId::new(id),
        fact: format!("Fact {id}."),
        probe: None,
        source: FactSource::UserStated,
        recorded_at: 7,
    }
}

/// Deterministic per-fact verdicts, so "no outcome lost" is a statement about
/// *which* outcome came back, not merely how many did.
fn verdict_for(id: &FactId) -> FactReport {
    let outcome = match id.as_str() {
        "f-1" => FactOutcome::Promoted,
        "f-2" => FactOutcome::Rejected {
            reason: "no improvement over the frozen probes".into(),
        },
        _ => FactOutcome::Failed {
            reason: "training job crashed".into(),
        },
    };
    FactReport {
        id: id.clone(),
        outcome,
    }
}

/// A submitter that behaves like the real remote one: once it has accepted a
/// batch the batch stays accepted, even when the reply never reaches sven.
#[derive(Default)]
struct FlakySubmitter {
    accepted: Mutex<Vec<FactId>>,
    drop_reply: Mutex<bool>,
    refuse: Mutex<bool>,
}

impl FlakySubmitter {
    fn accepted(&self) -> Vec<FactId> {
        self.accepted.lock().expect("accepted lock").clone()
    }

    /// The next `submit` accepts its batch and then loses the connection.
    fn drop_next_reply(&self) {
        *self.drop_reply.lock().expect("drop_reply lock") = true;
    }

    /// The next `submit` never even accepts the batch.
    fn refuse_next_submit(&self) {
        *self.refuse.lock().expect("refuse lock") = true;
    }
}

#[async_trait]
impl FactSubmitter for FlakySubmitter {
    async fn submit(&self, batch: &[PendingFactRecord]) -> anyhow::Result<Vec<FactReport>> {
        {
            let mut refuse = self.refuse.lock().expect("refuse lock");
            if *refuse {
                *refuse = false;
                anyhow::bail!("unreachable: the batch was never accepted");
            }
        }
        self.accepted
            .lock()
            .expect("accepted lock")
            .extend(batch.iter().map(|f| f.id.clone()));
        {
            let mut drop_reply = self.drop_reply.lock().expect("drop_reply lock");
            if *drop_reply {
                *drop_reply = false;
                anyhow::bail!("connection dropped after the batch was accepted");
            }
        }
        Ok(batch.iter().map(|f| verdict_for(&f.id)).collect())
    }

    async fn outcomes_for(&self, facts: &[FactId]) -> anyhow::Result<Vec<FactReport>> {
        let accepted = self.accepted.lock().expect("accepted lock");
        // A fact this submitter never accepted is omitted, never errored on:
        // silence is what tells the drain it is safe to send after all.
        Ok(facts
            .iter()
            .filter(|id| accepted.contains(id))
            .map(verdict_for)
            .collect())
    }
}

#[tokio::test]
async fn a_restart_mid_drain_resubmits_no_fact_twice_and_loses_no_outcome() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    for id in ["f-1", "f-2", "f-3"] {
        ledger.record_fact(&a_fact(id)).expect("record fact");
    }
    let cursor = dir.path().join("pending-facts.cursor.json");
    let submitter = FlakySubmitter::default();
    submitter.drop_next_reply();

    // Run one: the batch reaches the submitter, the reply never comes back.
    let err = PendingFactsDrain::new(ledger.clone(), &cursor, 8)
        .drain_once(&submitter)
        .await
        .expect_err("a dropped reply must surface as an error, never as success");
    assert!(
        err.to_string().contains("connection dropped"),
        "the submitter's own failure must not be swallowed: {err}"
    );
    assert_eq!(submitter.accepted().len(), 3, "the batch was accepted");

    // Restart: a brand-new drain reading the same persisted cursor, exactly as
    // a fresh process would.
    let drain = PendingFactsDrain::new(ledger.clone(), &cursor, 8);
    let reports = drain
        .drain_once(&submitter)
        .await
        .expect("the in-flight batch must be recoverable");

    assert_eq!(
        submitter.accepted(),
        vec![FactId::new("f-1"), FactId::new("f-2"), FactId::new("f-3")],
        "a restart must recover outcomes, never re-submit an accepted fact"
    );
    assert_eq!(
        reports,
        vec![
            verdict_for(&FactId::new("f-1")),
            verdict_for(&FactId::new("f-2")),
            verdict_for(&FactId::new("f-3")),
        ],
        "every fact in the interrupted batch must come back with its outcome"
    );

    // The cursor only moves once outcomes are known - and now it has.
    assert!(
        drain
            .drain_once(&submitter)
            .await
            .expect("an idle drain is not an error")
            .is_empty(),
        "a settled ledger has nothing left to drain"
    );
    assert_eq!(
        submitter.accepted().len(),
        3,
        "an idle drain must submit nothing at all"
    );
}

/// The in-flight marker is written *before* the batch is sent, so a crash in
/// that window leaves a marker for a batch the submitter never saw. Recovery
/// must notice that the submitter disclaims it and send it after all - a fact
/// nobody accepted has certainly not been trained on, and leaving it marked
/// in-flight forever would silently stop the drain for good.
#[tokio::test]
async fn a_batch_the_submitter_never_accepted_is_sent_afresh_not_stranded() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    ledger.record_fact(&a_fact("f-1")).expect("record fact");
    let cursor = dir.path().join("pending-facts.cursor.json");
    let submitter = FlakySubmitter::default();
    submitter.refuse_next_submit();

    let drain = PendingFactsDrain::new(ledger.clone(), &cursor, 8);
    drain
        .drain_once(&submitter)
        .await
        .expect_err("a refused submission is an error");
    assert!(
        submitter.accepted().is_empty(),
        "the batch never reached the submitter"
    );

    // Restart into the marker the refused pass left behind.
    let drain = PendingFactsDrain::new(ledger.clone(), &cursor, 8);
    assert!(
        drain
            .drain_once(&submitter)
            .await
            .expect("a disclaimed batch is not an error")
            .is_empty(),
        "nothing is settled: there was nothing to settle"
    );

    let reports = drain.drain_once(&submitter).await.expect("second pass");
    assert_eq!(
        reports,
        vec![verdict_for(&FactId::new("f-1"))],
        "the fact must still get its turn"
    );
    assert_eq!(submitter.accepted(), vec![FactId::new("f-1")]);
}

/// The drain is bounded by `batch_size` and resumes exactly where it stopped,
/// across separate processes sharing only the cursor file.
#[tokio::test]
async fn the_drain_hands_over_at_most_one_batch_per_pass_and_resumes_after_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    for id in ["f-1", "f-2", "f-3"] {
        ledger.record_fact(&a_fact(id)).expect("record fact");
    }
    let cursor = dir.path().join("pending-facts.cursor.json");
    let submitter = FlakySubmitter::default();

    let first = PendingFactsDrain::new(ledger.clone(), &cursor, 2)
        .drain_once(&submitter)
        .await
        .expect("first pass");
    assert_eq!(first.len(), 2, "a pass never exceeds the batch size");
    assert_eq!(
        submitter.accepted(),
        vec![FactId::new("f-1"), FactId::new("f-2")]
    );

    let second = PendingFactsDrain::new(ledger.clone(), &cursor, 2)
        .drain_once(&submitter)
        .await
        .expect("second pass");
    assert_eq!(
        second,
        vec![verdict_for(&FactId::new("f-3"))],
        "the next pass resumes at the fact the cursor stopped on"
    );
    assert_eq!(
        submitter.accepted(),
        vec![FactId::new("f-1"), FactId::new("f-2"), FactId::new("f-3")],
        "each fact goes out exactly once"
    );
}

/// A one-shot, non-interactive run (`sven --mode agent "learn from this
/// document"` in a shell script) has to finish learning before the process
/// exits: there is no next tick. `drain_all` is that entry point - it keeps
/// passing until the ledger is genuinely settled and returns every verdict it
/// collected, rather than the one batch `drain_once` is bounded to.
#[tokio::test]
async fn a_flush_settles_every_pending_fact_before_it_returns() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    for id in ["f-1", "f-2", "f-3"] {
        ledger.record_fact(&a_fact(id)).expect("record fact");
    }
    let cursor = dir.path().join("pending-facts.cursor.json");
    let submitter = FlakySubmitter::default();

    // One fact per batch, so a flush that stopped after the first pass - the
    // bound `drain_once` deliberately has - would come back with one report.
    let drain = PendingFactsDrain::new(ledger.clone(), &cursor, 1);
    let reports = drain.drain_all(&submitter).await.expect("flush");

    assert_eq!(
        reports,
        vec![
            verdict_for(&FactId::new("f-1")),
            verdict_for(&FactId::new("f-2")),
            verdict_for(&FactId::new("f-3")),
        ],
        "a flush returns every fact's real outcome, in ledger order"
    );
    assert_eq!(
        submitter.accepted(),
        vec![FactId::new("f-1"), FactId::new("f-2"), FactId::new("f-3")],
        "and still submits each fact exactly once"
    );
    assert!(
        drain.drain_all(&submitter).await.expect("idle flush").is_empty(),
        "a settled ledger flushes to nothing"
    );
}

/// The pass that clears a stranded in-flight marker settles nothing and
/// returns no reports - so a flush that stopped at the first empty pass would
/// leave the fact it disclaimed behind, unsubmitted, and report success.
#[tokio::test]
async fn a_flush_does_not_mistake_a_disclaimed_batch_for_an_empty_ledger() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    ledger.record_fact(&a_fact("f-1")).expect("record fact");
    let cursor = dir.path().join("pending-facts.cursor.json");
    let submitter = FlakySubmitter::default();
    submitter.refuse_next_submit();

    let drain = PendingFactsDrain::new(ledger.clone(), &cursor, 8);
    drain
        .drain_all(&submitter)
        .await
        .expect_err("a refused submission is still an error");

    let reports = drain.drain_all(&submitter).await.expect("flush");
    assert_eq!(
        reports,
        vec![verdict_for(&FactId::new("f-1"))],
        "the flush must clear the stranded marker AND then send the fact"
    );
}
