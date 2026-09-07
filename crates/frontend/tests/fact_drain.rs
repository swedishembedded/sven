// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The pending-facts drain only runs when a human turned it on.
//!
//! Draining the ledger is the one place where facts a user stated in private
//! leave the machine. That makes the config flag a consent boundary, not a
//! performance knob: absent config, an out-of-date config, or a config that
//! never mentions the feature must all mean "off", and the background task
//! must not exist at all rather than exist and quietly do nothing.
//!
//! Swedish Embedded AB implements solutions for consent-gated data egress in
//! autonomous agents for its clients. If your team needs expertise in keeping
//! private context on the machine it was stated on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use sven_config::Config;
use sven_frontend::spawn_fact_drain;
use sven_memory::{
    FactOutcome, FactReport, FactSubmitter, PendingFactRecord, PendingFactsLedger,
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
                outcome: FactOutcome::Promoted,
            })
            .collect())
    }

    async fn outcomes_for(&self, _facts: &[FactId]) -> anyhow::Result<Vec<FactReport>> {
        Ok(Vec::new())
    }
}

fn a_fact(id: &str) -> PendingFactRecord {
    PendingFactRecord {
        id: FactId::new(id),
        fact: format!("Fact {id}."),
        source: FactSource::UserStated,
        recorded_at: 7,
    }
}

/// Waits for `want` facts to have been submitted, or gives up.
async fn wait_for(submitter: &CountingSubmitter, want: usize) -> Vec<FactId> {
    for _ in 0..100 {
        let seen = submitter.submitted();
        if seen.len() >= want {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    submitter.submitted()
}

#[tokio::test]
async fn the_fact_drain_stays_off_until_the_config_flag_turns_it_on() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    ledger.record_fact(&a_fact("f-1")).expect("record fact");
    let submitter = Arc::new(CountingSubmitter::default());

    let mut config = Config::default();
    assert!(
        !config.tools.memory.learning.submit_facts,
        "a default config must never ship facts off the machine"
    );
    assert!(
        spawn_fact_drain(&config, ledger.clone(), submitter.clone()).is_none(),
        "with the flag off there must be no background task at all"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        submitter.submitted().is_empty(),
        "nothing may be submitted while the drain is off"
    );

    config.tools.memory.learning.submit_facts = true;
    let handle = spawn_fact_drain(&config, ledger.clone(), submitter.clone())
        .expect("the flag being on must spawn the drain");

    assert_eq!(
        wait_for(&submitter, 1).await,
        vec![FactId::new("f-1")],
        "the enabled drain must reach the ledger's pending facts"
    );
    handle.abort();
}

/// The ledger lives at one well-known path per machine and the drain's
/// exactly-once guarantee rests on a cursor with a single owner. Two `sven`
/// windows open at once must therefore not become two drains submitting the
/// same batch twice - the exact double-training the ledger exists to prevent.
#[tokio::test]
async fn a_second_drain_over_the_same_ledger_refuses_to_start() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    ledger.record_fact(&a_fact("f-1")).expect("record fact");
    let submitter = Arc::new(CountingSubmitter::default());

    let mut config = Config::default();
    config.tools.memory.learning.submit_facts = true;

    let first = spawn_fact_drain(&config, ledger.clone(), submitter.clone())
        .expect("the first drain takes the lock");
    assert!(
        spawn_fact_drain(&config, ledger.clone(), submitter.clone()).is_none(),
        "a second drain over the same cursor must refuse to start"
    );

    // The one drain that did start is still the one doing the work.
    assert_eq!(wait_for(&submitter, 1).await, vec![FactId::new("f-1")]);
    first.abort();
}
