// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The durable pending-facts ledger.
//!
//! An append-only, hash-chained JSONL log (see [`sven_chain`]) holding the two
//! record kinds the continuous-learning loop needs to reason about provenance:
//!
//! * [`DocumentRecord`] - an artifact a human handed over, identified by the
//!   digest computed over the bytes that were actually read. The presence of
//!   such a record is what makes facts extracted from that digest admissible.
//! * [`PendingFactRecord`] - a fact whose provenance cleared the admissibility
//!   rule and which is therefore a candidate for training.
//!
//! Semantic memory answers "what do I know this session". This ledger answers
//! "what did a human actually stand behind, and on what basis" - a strictly
//! narrower and slower-moving set.
//!
//! # Honest limitation
//!
//! `sven-chain` is **not** tamper-evident against an attacker with local write
//! access: there is no keyed MAC and no external anchor, so anyone who can edit
//! the file can rewrite any entry and re-derive every subsequent hash. What the
//! chain buys here is ordering, provenance and crash-safety - not integrity
//! against local compromise. Do not build a security claim on the chain alone.
//!
//! Swedish Embedded AB implements solutions for auditable knowledge pipelines
//! for its clients. If your team needs expertise in append-only provenance
//! ledgers then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sven_vocab::provenance::{ContentDigest, FactId, FactSource};

/// An artifact a human handed over for the agent to learn from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRecord {
    /// Digest computed by the ingesting tool over the bytes it read.
    pub digest: ContentDigest,
    /// Where the artifact came from (path or URL, as given).
    pub uri: String,
    /// Unix seconds at which it was ingested.
    pub ingested_at: u64,
}

/// The question a fact is scored by, frozen when the fact was extracted.
///
/// Training on a fact is only defensible if something can afterwards decide
/// whether the model *learned* it, and the only honest decider is a question
/// the extractor wrote while reading the source - never one derived later from
/// the fact itself, which tests the derivation rather than the knowledge. So
/// the probe is captured at the same moment as the fact and travels with it.
///
/// The question must not appear inside the fact it belongs to: a probe whose
/// own question sits in the row the model is trained on is a memorisation test
/// wearing a generalisation test's clothes. The *answer* deliberately may -
/// "the 3rd relay closes at 13 volts" is exactly the row that has to teach the
/// answer "13 volts", so an answer-absence rule would reject every batch that
/// could ever work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenProbe {
    /// What the trained model is asked. Never trained on.
    pub question: String,
    /// The answer `question` must elicit, verbatim.
    pub expected_answer: String,
}

/// A fact accepted into the ledger as a candidate for training.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFactRecord {
    /// Stable identifier for this fact.
    pub id: FactId,
    /// The assertion itself - what would be trained on.
    pub fact: String,
    /// How the fact is scored, if the extractor froze a probe for it.
    ///
    /// `#[serde(default)]`, so a ledger written before probes existed still
    /// reads: those facts are knowledge with no way to score it, and a
    /// submitter that needs a probe reports exactly that rather than guessing
    /// one up.
    #[serde(default)]
    pub probe: Option<FrozenProbe>,
    /// Where it came from. Attached by the resolution path, never by the model.
    pub source: FactSource,
    /// Unix seconds at which it was recorded.
    pub recorded_at: u64,
}

/// One entry in the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerEntry {
    /// See [`DocumentRecord`].
    Document(DocumentRecord),
    /// See [`PendingFactRecord`].
    PendingFact(PendingFactRecord),
}

/// Why a ledger operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The underlying chain could not be read or was broken.
    #[error("pending-facts ledger is unreadable: {0}")]
    Chain(#[from] sven_chain::ChainError),
    /// The append failed.
    #[error("pending-facts ledger append failed: {0}")]
    Io(#[from] std::io::Error),
    /// An entry could not be encoded or decoded.
    #[error("pending-facts ledger entry is malformed: {0}")]
    Encoding(#[from] serde_json::Error),
}

/// Handle to the pending-facts ledger at a fixed path.
///
/// Cheap to clone; all state lives on disk. Every method performs **blocking**
/// filesystem I/O - call from a blocking context inside async code.
#[derive(Clone, Debug)]
pub struct PendingFactsLedger {
    path: PathBuf,
}

impl PendingFactsLedger {
    /// A handle to the ledger stored at `path`. The file is created on the
    /// first append.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A handle to the ledger at its conventional location, beside the
    /// semantic memory store (see [`Self::default_path`]).
    #[must_use]
    pub fn at_default_path() -> Self {
        Self::new(Self::default_path())
    }

    /// The conventional on-disk location, in the same directory
    /// [`crate::SqliteMemoryStore::open`] defaults to.
    #[must_use]
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/sven/memory/pending-facts.jsonl")
    }

    /// The on-disk path of this ledger.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends a handed-over document, making facts extracted from its digest
    /// admissible.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the entry cannot be encoded or appended.
    pub fn record_document(&self, record: &DocumentRecord) -> Result<(), LedgerError> {
        self.append(&LedgerEntry::Document(record.clone()))
    }

    /// Appends an admissible fact.
    ///
    /// Admissibility is decided by the caller that can see the provenance (see
    /// `AssimilateFactTool`); this method records the decision, it does not
    /// make it.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the entry cannot be encoded or appended.
    pub fn record_fact(&self, record: &PendingFactRecord) -> Result<(), LedgerError> {
        self.append(&LedgerEntry::PendingFact(record.clone()))
    }

    /// Every entry, in append order.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the chain is broken or an entry cannot be
    /// decoded.
    pub fn entries(&self) -> Result<Vec<LedgerEntry>, LedgerError> {
        let mut out = Vec::new();
        for line in sven_chain::read_chain(&self.path)? {
            out.push(serde_json::from_value(line.entry)?);
        }
        Ok(out)
    }

    /// The digests of every document a human actually handed over.
    ///
    /// # Errors
    ///
    /// Same as [`Self::entries`].
    pub fn ingested_document_digests(&self) -> Result<HashSet<ContentDigest>, LedgerError> {
        Ok(self
            .entries()?
            .into_iter()
            .filter_map(|e| match e {
                LedgerEntry::Document(d) => Some(d.digest),
                LedgerEntry::PendingFact(_) => None,
            })
            .collect())
    }

    /// Every fact admitted so far, in append order.
    ///
    /// # Errors
    ///
    /// Same as [`Self::entries`].
    pub fn pending_facts(&self) -> Result<Vec<PendingFactRecord>, LedgerError> {
        Ok(self
            .entries()?
            .into_iter()
            .filter_map(|e| match e {
                LedgerEntry::PendingFact(f) => Some(f),
                LedgerEntry::Document(_) => None,
            })
            .collect())
    }

    fn append(&self, entry: &LedgerEntry) -> Result<(), LedgerError> {
        let value = serde_json::to_value(entry)?;
        sven_chain::append_chain(&self.path, vec![value])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_fact(id: &str) -> PendingFactRecord {
        PendingFactRecord {
            id: FactId::new(id),
            fact: "The CAN bus runs at 500 kbit/s.".to_string(),
            probe: Some(FrozenProbe {
                question: "How fast does the vehicle network run?".to_string(),
                expected_answer: "500 kbit/s".to_string(),
            }),
            source: FactSource::UserStated,
            recorded_at: 7,
        }
    }

    #[test]
    fn documents_and_facts_share_one_verifiable_chain() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));

        assert!(ledger.entries().expect("empty ledger reads clean").is_empty());

        ledger
            .record_document(&DocumentRecord {
                digest: ContentDigest::from_hex("abc123"),
                uri: "file:///spec.md".into(),
                ingested_at: 5,
            })
            .expect("record document");
        ledger.record_fact(&a_fact("f-1")).expect("record fact");

        assert_eq!(
            sven_chain::verify_chain(ledger.path()).expect("verify"),
            2,
            "both record kinds extend the same hash chain"
        );
        assert_eq!(
            ledger.ingested_document_digests().expect("digests"),
            HashSet::from([ContentDigest::from_hex("abc123")])
        );
        assert_eq!(ledger.pending_facts().expect("facts"), vec![a_fact("f-1")]);
    }

    /// The ledger is append-only and hash-chained: entries written before
    /// probes existed can never be rewritten to carry one, so they have to
    /// keep reading. A fact with no probe is knowledge that cannot be scored,
    /// not a corrupt entry.
    #[test]
    fn a_fact_recorded_before_probes_existed_still_reads_as_one_without_a_probe() {
        let record: PendingFactRecord = serde_json::from_str(
            r#"{"id":"f-1","fact":"The CAN bus runs at 500 kbit/s.",
                "source":{"kind":"user_stated"},"recorded_at":7}"#,
        )
        .expect("a pre-probe ledger entry must still decode");
        assert_eq!(record.probe, None);
    }
}
