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
    FactOutcome, FactReport, FactSubmitter, GateNumbers, PendingFactRecord, PendingFactsLedger,
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

fn a_fact(id: &str) -> PendingFactRecord {
    PendingFactRecord {
        id: FactId::new(id),
        fact: format!("Fact {id}."),
        probe: None,
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

/// The submitter the drain uses is a config value, not a compile-time choice -
/// which is the whole reason `FactSubmitter` is generic. `local` is the
/// default because sven + brain on one machine is now the primary learning
/// loop, but the one thing it cannot guess is where `brain serve
/// --watch-adapters` is pointed, so a `local` submitter without that directory
/// declines to start rather than training a model nothing serves.
#[tokio::test]
async fn the_default_drain_selects_its_submitter_from_config_and_refuses_a_broken_one() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    ledger.record_fact(&a_fact("f-1")).expect("record fact");

    let mut config = Config::default();
    assert_eq!(
        config.tools.memory.learning.submitter, "local",
        "the sven+brain path is the default one, not whale's"
    );
    assert!(
        sven_frontend::spawn_default_fact_drain(&config, ledger.clone()).is_none(),
        "the consent flag still gates everything: off means no task at all"
    );

    config.tools.memory.learning.submit_facts = true;
    assert!(
        sven_frontend::spawn_default_fact_drain(&config, ledger.clone()).is_none(),
        "a local submitter with no adapter_dir must decline, not guess a directory"
    );

    config.tools.memory.learning.submitter = "none".to_string();
    assert!(
        sven_frontend::spawn_default_fact_drain(&config, ledger).is_none(),
        "and an explicitly empty selection spawns nothing"
    );
}
