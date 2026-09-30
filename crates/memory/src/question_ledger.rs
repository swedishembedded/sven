// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The durable parked-question ledger.
//!
//! An append-only, hash-chained JSONL log (see [`sven_chain`]) of two kinds of
//! entry: a question is [`QuestionLedgerEntry::Asked`] the moment
//! `UserExecutor` parks it (see `sven_executors::user::ParkedQuestion`), and
//! later
//! [`QuestionLedgerEntry::Answered`] by whatever surface a human used to
//! reply - possibly a different process entirely, possibly long after the
//! agent that asked it has exited.
//!
//! This ledger only *records* questions and answers; it does not resume a
//! parked kernel. Posting the matching `Event::HumanAnswered` back into the
//! right session is the caller's job (see `sven-hsm::event::Event`).
//!
//! # Honest limitation
//!
//! `sven-chain` is **not** tamper-evident against an attacker with local write
//! access: there is no keyed MAC and no external anchor, so anyone who can edit
//! the file can rewrite any entry and re-derive every subsequent hash. What the
//! chain buys here is ordering, provenance and crash-safety - not integrity
//! against local compromise. Do not build a security claim on the chain alone.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sven_hsm::{QuestionId, ToolCallId};

/// A question parked, the moment it was parked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAskedRecord {
    /// Identifies this question; a later `Answered` record matches by this.
    pub question_id: QuestionId,
    /// The tool call this question was asked on behalf of.
    pub call_id: ToolCallId,
    /// The question text shown to the human.
    pub prompt: String,
    /// Offered choices, if any (empty for a free-form question).
    pub options: Vec<String>,
    /// Unix seconds at which it was parked.
    pub asked_at: u64,
}

/// A human's answer to a previously parked question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnsweredRecord {
    /// Matches the [`QuestionAskedRecord::question_id`] being answered.
    pub question_id: QuestionId,
    /// The human's answer, verbatim.
    pub answer: String,
    /// Unix seconds at which it was answered.
    pub answered_at: u64,
}

/// One entry in the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionLedgerEntry {
    /// See [`QuestionAskedRecord`].
    Asked(QuestionAskedRecord),
    /// See [`QuestionAnsweredRecord`].
    Answered(QuestionAnsweredRecord),
}

/// Why a ledger operation failed.
#[derive(Debug, thiserror::Error)]
pub enum QuestionLedgerError {
    /// The underlying chain could not be read or was broken.
    #[error("question ledger is unreadable: {0}")]
    Chain(#[from] sven_chain::ChainError),
    /// The append failed.
    #[error("question ledger append failed: {0}")]
    Io(#[from] std::io::Error),
    /// An entry could not be encoded or decoded.
    #[error("question ledger entry is malformed: {0}")]
    Encoding(#[from] serde_json::Error),
}

/// Handle to the parked-question ledger at a fixed path.
///
/// Cheap to clone; all state lives on disk. Every method performs
/// **blocking** filesystem I/O - call from a blocking context inside async
/// code.
#[derive(Clone, Debug)]
pub struct QuestionLedger {
    path: PathBuf,
}

impl QuestionLedger {
    /// A handle to the ledger stored at `path`. The file is created on the
    /// first append.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A handle to the ledger at its conventional location.
    #[must_use]
    pub fn at_default_path() -> Self {
        Self::new(Self::default_path())
    }

    /// The conventional on-disk location, beside the semantic memory store.
    #[must_use]
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/sven/memory/questions.jsonl")
    }

    /// The on-disk path of this ledger.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends a parked question.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionLedgerError`] when the entry cannot be encoded or
    /// appended.
    pub fn record_asked(&self, record: &QuestionAskedRecord) -> Result<(), QuestionLedgerError> {
        self.append(&QuestionLedgerEntry::Asked(record.clone()))
    }

    /// Appends a human's answer.
    ///
    /// Does not check whether `question_id` matches an existing `Asked`
    /// record - see [`Self::pending_questions`], which is where that
    /// matching happens, so a caller can decide how to handle an answer to a
    /// question this ledger never recorded (e.g. one asked before the ledger
    /// existed) rather than have it silently rejected here.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionLedgerError`] when the entry cannot be encoded or
    /// appended.
    pub fn record_answered(
        &self,
        record: &QuestionAnsweredRecord,
    ) -> Result<(), QuestionLedgerError> {
        self.append(&QuestionLedgerEntry::Answered(record.clone()))
    }

    /// Every entry, in append order.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionLedgerError`] when the chain is broken or an entry
    /// cannot be decoded.
    pub fn entries(&self) -> Result<Vec<QuestionLedgerEntry>, QuestionLedgerError> {
        let mut out = Vec::new();
        for line in sven_chain::read_chain(&self.path)? {
            out.push(serde_json::from_value(line.entry)?);
        }
        Ok(out)
    }

    /// Every parked question with no matching answer yet, in the order they
    /// were asked.
    ///
    /// # Errors
    ///
    /// Same as [`Self::entries`].
    pub fn pending_questions(&self) -> Result<Vec<QuestionAskedRecord>, QuestionLedgerError> {
        let entries = self.entries()?;
        let answered: std::collections::HashSet<QuestionId> = entries
            .iter()
            .filter_map(|e| match e {
                QuestionLedgerEntry::Answered(a) => Some(a.question_id),
                QuestionLedgerEntry::Asked(_) => None,
            })
            .collect();
        Ok(entries
            .into_iter()
            .filter_map(|e| match e {
                QuestionLedgerEntry::Asked(q) if !answered.contains(&q.question_id) => Some(q),
                _ => None,
            })
            .collect())
    }

    fn append(&self, entry: &QuestionLedgerEntry) -> Result<(), QuestionLedgerError> {
        let value = serde_json::to_value(entry)?;
        sven_chain::append_chain(&self.path, vec![value])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(question_id: QuestionId) -> QuestionAskedRecord {
        QuestionAskedRecord {
            question_id,
            call_id: ToolCallId::new(),
            prompt: "Which framework?".into(),
            options: vec!["Axum".into(), "Actix".into()],
            asked_at: 7,
        }
    }

    #[test]
    fn a_fresh_ledger_reads_empty() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = QuestionLedger::new(dir.path().join("questions.jsonl"));
        assert!(ledger
            .entries()
            .expect("empty ledger reads clean")
            .is_empty());
        assert!(ledger
            .pending_questions()
            .expect("empty ledger reads clean")
            .is_empty());
    }

    #[test]
    fn an_asked_question_is_pending_until_answered() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = QuestionLedger::new(dir.path().join("questions.jsonl"));
        let question_id = QuestionId::new();
        ledger
            .record_asked(&asked(question_id))
            .expect("record asked");

        let pending = ledger.pending_questions().expect("read pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].question_id, question_id);

        ledger
            .record_answered(&QuestionAnsweredRecord {
                question_id,
                answer: "Axum".into(),
                answered_at: 9,
            })
            .expect("record answered");

        assert!(
            ledger.pending_questions().expect("read pending").is_empty(),
            "an answered question must not still be pending"
        );
    }

    #[test]
    fn unrelated_questions_stay_independent() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = QuestionLedger::new(dir.path().join("questions.jsonl"));
        let a = QuestionId::new();
        let b = QuestionId::new();
        ledger.record_asked(&asked(a)).expect("record a");
        ledger.record_asked(&asked(b)).expect("record b");
        ledger
            .record_answered(&QuestionAnsweredRecord {
                question_id: a,
                answer: "Axum".into(),
                answered_at: 9,
            })
            .expect("answer a");

        let pending = ledger.pending_questions().expect("read pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].question_id, b,
            "only the unanswered question remains pending"
        );
    }

    #[test]
    fn asked_and_answered_share_one_verifiable_chain() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = QuestionLedger::new(dir.path().join("questions.jsonl"));
        let question_id = QuestionId::new();
        ledger
            .record_asked(&asked(question_id))
            .expect("record asked");
        ledger
            .record_answered(&QuestionAnsweredRecord {
                question_id,
                answer: "Axum".into(),
                answered_at: 9,
            })
            .expect("record answered");

        assert_eq!(
            sven_chain::verify_chain(ledger.path()).expect("verify"),
            2,
            "both record kinds extend the same hash chain"
        );
    }

    #[test]
    fn an_answer_to_an_unknown_question_is_recorded_but_matches_nothing() {
        // A question asked before this ledger existed, or by a process that
        // never recorded it - the answer is still durably recorded, it just
        // never resolves a pending entry. Silently rejecting it would lose
        // the human's answer for no benefit.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let ledger = QuestionLedger::new(dir.path().join("questions.jsonl"));
        ledger
            .record_answered(&QuestionAnsweredRecord {
                question_id: QuestionId::new(),
                answer: "whatever".into(),
                answered_at: 1,
            })
            .expect("record answered");
        assert_eq!(ledger.entries().expect("read").len(), 1);
        assert!(ledger.pending_questions().expect("read pending").is_empty());
    }
}
