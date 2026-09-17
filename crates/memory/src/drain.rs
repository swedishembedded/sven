// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The pending-facts drain: ledger tail → batch → submitter → outcomes.
//!
//! [`PendingFactsLedger`] accumulates the facts whose provenance cleared the
//! admissibility gate. This module is the other end of that pipe: it reads the
//! ledger tail past a persisted cursor, hands a bounded batch to a
//! [`FactSubmitter`], and only then advances the cursor.
//!
//! # Why the trait is bidirectional
//!
//! A submit-only submitter cannot be made exactly-once. Advance the cursor
//! before the result is known and every outcome is lost; advance it after and
//! any crash in between re-submits facts that were already accepted - which
//! means training on them twice. So [`FactSubmitter`] has a second method:
//! [`FactSubmitter::outcomes_for`], which reports the fate of a batch that was
//! already handed over. The drain writes an *in-flight* marker to its cursor
//! file before the batch leaves the process, and a restart that finds one
//! recovers its outcomes instead of re-sending it.
//!
//! That gives the honest guarantee: **at-most-once submission, exactly-once
//! settlement** - assuming the submitter keeps its own end of the bargain and
//! remembers what it accepted. The per-fact verdicts (promoted / rejected /
//! failed) are what flow back to the user; the ledger is not rewritten.
//!
//! The marker is written before the batch is sent, so a crash in that window
//! leaves a marker for a batch the submitter never saw. That is why
//! [`FactSubmitter::outcomes_for`] *omits* facts it has no record of rather
//! than failing: disclaiming a fact proves it was never trained on, which is
//! exactly the evidence the drain needs to send it after all instead of
//! stalling on it forever.
//!
//! # What this is not
//!
//! Not an HSM `Effect`. The kernel owns pure transitions, not a training
//! pipeline; the drain runs as a background task in the wiring tier
//! (`sven_frontend`) behind a config flag. And any concrete remote
//! [`FactSubmitter`] lives outside this repo - this trait is deliberately
//! generic and says nothing about jobs, nodes or payments.
//!
//! # Honest limitations
//!
//! The cursor inherits [`PendingFactsLedger`]'s: `sven-chain` is not
//! tamper-evident against an attacker with local write access, and neither is
//! the cursor file beside it. Both give ordering and crash-safety, not
//! integrity against local compromise. A single drain owns a given cursor
//! file; two concurrent drains sharing one would race each other's writes.
//!
//! The cursor is replaced by a synced temporary file and a rename, and the
//! containing directory is synced best-effort afterwards. Where that
//! directory sync is unavailable (notably Windows) a power loss in the
//! rename window can lose the newest cursor, which degrades the guarantee to
//! at-least-once for the batch it described.
//!
//! Swedish Embedded AB implements solutions for exactly-once hand-off between
//! an agent's knowledge ledger and its training pipeline for its clients. If
//! your team needs expertise in crash-safe drain protocols then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use sven_vocab::provenance::FactId;

use crate::ledger::{LedgerEntry, LedgerError, PendingFactRecord, PendingFactsLedger};

/// The gate's own numbers for the cycle a fact was decided in, when the study
/// ran far enough to produce them. Every field is optional: a study that
/// crashed before scoring, or a brain report that has not grown a field yet,
/// still produces a verdict - just not every number behind it.
///
/// Carried on both [`FactOutcome::Promoted`] and [`FactOutcome::Rejected`] so
/// a caller (a script, `sven learn flush --json`, `F1`'s own before/after
/// measurement) can read the gate's evidence directly instead of parsing it
/// back out of a human-readable `reason` string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GateNumbers {
    /// The incumbent (pre-training) arm's pass rate on this cycle's frozen
    /// probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_pass_rate: Option<f64>,
    /// The candidate (post-training) arm's pass rate on the same probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_training_pass_rate: Option<f64>,
    /// The gate's sign-test p-value for this cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    /// The gate's effect size for this cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_size: Option<f64>,
}

/// What became of one submitted fact.
///
/// Reported per fact, never per batch: a batch routinely lands partially, and
/// "the batch was accepted" is not an answer the user can act on.
///
/// Does not derive `Eq`: [`GateNumbers`] carries `f64`, which has none. Use
/// `PartialEq`/`assert_eq!` as before - `f64: PartialEq` is enough for every
/// existing comparison, none of which compares against `NaN`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FactOutcome {
    /// Trained on and promoted - the model now knows it. Carries the gate's
    /// own numbers for the cycle that promoted, when the study reported them.
    Promoted {
        /// The gate's evidence for this promotion.
        #[serde(default)]
        numbers: GateNumbers,
    },
    /// Evaluated and deliberately not promoted (e.g. it failed the gate).
    Rejected {
        /// Why it was turned down, for the user.
        reason: String,
        /// The gate's evidence for this rejection, when the study reported
        /// it - the same numbers `reason`'s free text already describes, in
        /// a form a machine reader does not have to parse back out.
        #[serde(default)]
        numbers: GateNumbers,
    },
    /// The pipeline itself failed for this fact; nothing was decided.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// One fact's reported outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactReport {
    /// The fact this verdict is about.
    pub id: FactId,
    /// The verdict.
    pub outcome: FactOutcome,
}

/// Hands admitted facts to whatever trains on them, and reports back.
///
/// Implementations may live outside this repo entirely. Two rules make
/// the drain's exactly-once settlement possible, and an implementation that
/// breaks either of them breaks the guarantee:
///
/// 1. [`Self::submit`] must be durable before it returns success - a fact it
///    reported on is a fact it has taken responsibility for;
/// 2. [`Self::outcomes_for`] must answer for any batch a previous
///    [`Self::submit`] accepted, **including one whose reply never reached the
///    caller**, and must not treat being asked again as a new submission.
#[async_trait]
pub trait FactSubmitter: Send + Sync {
    /// Submits `batch` and returns one [`FactReport`] per fact in it.
    ///
    /// # Errors
    ///
    /// Any failure to submit *or* to report. The drain cannot tell the two
    /// apart - which is exactly why [`Self::outcomes_for`] exists.
    async fn submit(&self, batch: &[PendingFactRecord]) -> anyhow::Result<Vec<FactReport>>;

    /// Reports the outcomes of facts a previous [`Self::submit`] accepted.
    ///
    /// Called only on the recovery path, after a crash or a lost reply. It
    /// must never cause the facts to be submitted a second time.
    ///
    /// **Omit any fact this submitter has no record of.** Silence means "never
    /// accepted, therefore never trained on", and the drain relies on it to
    /// re-send a batch that a crash interrupted before it was ever sent.
    /// Returning an error for such a fact instead would strand it forever.
    ///
    /// # Errors
    ///
    /// When the outcomes cannot be established *at all* (the remote end is
    /// unreachable, say); the drain keeps the batch in flight and asks again
    /// later.
    async fn outcomes_for(&self, facts: &[FactId]) -> anyhow::Result<Vec<FactReport>>;
}

/// Why a drain pass could not complete.
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    /// The ledger could not be read.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// The cursor file could not be read or written.
    #[error("drain cursor at {path} is unusable: {source}")]
    Cursor {
        /// The cursor file.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The cursor file exists but is not a cursor.
    #[error("drain cursor at {path} is malformed: {source}")]
    CursorEncoding {
        /// The cursor file.
        path: PathBuf,
        /// The decode failure.
        source: serde_json::Error,
    },
    /// The cursor points past the end of the ledger: the ledger was truncated
    /// or rewritten underneath the drain. Resuming would re-submit facts, so
    /// the drain refuses instead.
    #[error(
        "drain cursor is at entry {cursor} but the ledger holds only {entries}: \
         the ledger was truncated or rewritten"
    )]
    CursorAheadOfLedger {
        /// Where the cursor points.
        cursor: usize,
        /// How many entries the ledger actually has.
        entries: usize,
    },
    /// The submitter failed to submit, or to report.
    #[error("fact submitter failed: {0}")]
    Submitter(anyhow::Error),
    /// The submitter answered, but not about every fact it was given. The
    /// cursor stays put rather than advancing over a fact with no verdict.
    #[error("fact submitter reported no outcome for {} fact(s): {}", .missing.len(), .missing.iter().map(FactId::as_str).collect::<Vec<_>>().join(", "))]
    IncompleteOutcomes {
        /// The facts left unaccounted for.
        missing: Vec<FactId>,
    },
    /// A blocking ledger/cursor operation panicked.
    #[error("drain worker panicked: {0}")]
    Worker(String),
}

/// A batch handed to the submitter whose outcomes were never recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InFlightBatch {
    /// Ledger entry count this batch settles up to, once its outcomes land.
    through: usize,
    /// The facts in the batch, in submission order.
    facts: Vec<FactId>,
}

/// The persisted drain position.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DrainCursor {
    /// Ledger entries whose facts have known outcomes.
    #[serde(default)]
    settled: usize,
    /// See [`InFlightBatch`]. Written *before* a batch is submitted.
    #[serde(default)]
    in_flight: Option<InFlightBatch>,
}

/// Reads the pending-facts ledger tail and hands it to a [`FactSubmitter`].
///
/// Cheap to clone; all state lives on disk. See the module docs for the
/// crash-safety protocol.
#[derive(Clone, Debug)]
pub struct PendingFactsDrain {
    ledger: PendingFactsLedger,
    cursor_path: PathBuf,
    batch_size: usize,
}

impl PendingFactsDrain {
    /// A drain over `ledger`, persisting its position to `cursor_path`.
    ///
    /// `batch_size` is clamped to at least 1 - a zero-sized batch would submit
    /// nothing forever.
    #[must_use]
    pub fn new(
        ledger: PendingFactsLedger,
        cursor_path: impl AsRef<Path>,
        batch_size: usize,
    ) -> Self {
        Self {
            ledger,
            cursor_path: cursor_path.as_ref().to_path_buf(),
            batch_size: batch_size.max(1),
        }
    }

    /// The conventional cursor location for `ledger`: its path plus
    /// `.cursor.json`.
    #[must_use]
    pub fn default_cursor_path(ledger: &PendingFactsLedger) -> PathBuf {
        let mut os = ledger.path().as_os_str().to_owned();
        os.push(".cursor.json");
        PathBuf::from(os)
    }

    /// The cursor file this drain persists to.
    #[must_use]
    pub fn cursor_path(&self) -> &Path {
        &self.cursor_path
    }

    /// Runs one pass: recover an interrupted batch, or submit the next one.
    ///
    /// Returns the outcomes settled by this pass - empty when the ledger has
    /// nothing new, or when an interrupted batch turned out never to have
    /// reached the submitter (the next pass sends it afresh). A pass settles
    /// at most one batch, so a caller with a backlog simply calls again.
    ///
    /// # Errors
    ///
    /// See [`DrainError`]. On every error the cursor is left exactly as it
    /// was, so the next pass retries the same work rather than skipping it.
    pub async fn drain_once(
        &self,
        submitter: &dyn FactSubmitter,
    ) -> Result<Vec<FactReport>, DrainError> {
        let cursor = self.read_cursor().await?;
        let entries = self.read_ledger().await?;

        if let Some(in_flight) = &cursor.in_flight {
            guard_cursor(in_flight.through, entries.len())?;
            return self.recover(submitter, in_flight, cursor.settled).await;
        }

        guard_cursor(cursor.settled, entries.len())?;
        let (facts, through) = next_batch(&entries, cursor.settled, self.batch_size);
        if facts.is_empty() {
            // Nothing to submit, but document entries may have been scanned
            // past; remember that so the next pass does not rescan them.
            if through > cursor.settled {
                self.write_cursor(DrainCursor {
                    settled: through,
                    in_flight: None,
                })
                .await?;
            }
            return Ok(Vec::new());
        }

        let in_flight = InFlightBatch {
            through,
            facts: facts.iter().map(|f| f.id.clone()).collect(),
        };
        // Durable before the batch leaves the process: a crash after this
        // point is recoverable via `outcomes_for`, and a crash before it means
        // nothing was ever submitted.
        self.write_cursor(DrainCursor {
            settled: cursor.settled,
            in_flight: Some(in_flight.clone()),
        })
        .await?;

        let reports = submitter
            .submit(&facts)
            .await
            .map_err(DrainError::Submitter)?;
        self.settle(&in_flight, reports).await
    }

    /// Drains until nothing is left, and returns every outcome it settled.
    ///
    /// [`Self::drain_once`] is bounded to one batch because the background
    /// task that calls it has a next tick. A one-shot, non-interactive run
    /// does not: `sven --mode agent "learn from this document"` in a shell
    /// script exits when the prompt is answered, and a fact still sitting in
    /// the ledger at that moment is a fact nothing will ever come back for. So
    /// this keeps passing until the ledger is genuinely settled, and blocks on
    /// the submitter's real outcomes throughout - never on a timer, which
    /// would be a guess about how long training takes.
    ///
    /// "Genuinely settled" is not "a pass returned no reports": the pass that
    /// clears a stranded in-flight marker settles nothing and reports nothing,
    /// and stopping there would leave the fact it just disclaimed unsubmitted
    /// while reporting success. The loop therefore ends only when a pass both
    /// reports nothing *and* leaves the cursor untouched - the one state that
    /// means there was no work, rather than work that produced no verdict.
    ///
    /// Additive: the periodic drain is unchanged and both may be used, though
    /// not at the same time - the single-drain lock in `sven-frontend` is what
    /// keeps two of them off one cursor.
    ///
    /// # Errors
    ///
    /// The first [`DrainError`] any pass hits, with everything settled up to
    /// that point discarded from the return value but *not* from the cursor:
    /// those outcomes are recorded on disk and a later pass will not redo them.
    /// Callers that need the partial list should call [`Self::drain_once`] in
    /// their own loop.
    pub async fn drain_all(
        &self,
        submitter: &dyn FactSubmitter,
    ) -> Result<Vec<FactReport>, DrainError> {
        let mut all = Vec::new();
        loop {
            let before = self.read_cursor().await?;
            let reports = self.drain_once(submitter).await?;
            let settled_something = !reports.is_empty();
            all.extend(reports);
            if !settled_something && self.read_cursor().await? == before {
                return Ok(all);
            }
        }
    }

    /// Resolves a batch the previous run left in flight, without ever
    /// re-submitting a fact the submitter took responsibility for.
    ///
    /// The submitter omitting a fact is a positive statement that it never
    /// accepted it - so a batch it disclaims *entirely* is one that never left
    /// the process (the crash landed between writing the marker and sending),
    /// and the marker is simply dropped so the normal path sends it afresh.
    /// A partially disclaimed batch is genuinely ambiguous and is refused
    /// rather than guessed at.
    async fn recover(
        &self,
        submitter: &dyn FactSubmitter,
        batch: &InFlightBatch,
        settled: usize,
    ) -> Result<Vec<FactReport>, DrainError> {
        let reports = submitter
            .outcomes_for(&batch.facts)
            .await
            .map_err(DrainError::Submitter)?;
        if reports.is_empty() {
            self.write_cursor(DrainCursor {
                settled,
                in_flight: None,
            })
            .await?;
            return Ok(Vec::new());
        }
        self.settle(batch, reports).await
    }

    /// Records `reports` as the final word on `batch` and advances the cursor.
    ///
    /// Refuses to advance over a fact the submitter said nothing about: an
    /// unaccounted fact is precisely the outcome the drain exists not to lose.
    async fn settle(
        &self,
        batch: &InFlightBatch,
        reports: Vec<FactReport>,
    ) -> Result<Vec<FactReport>, DrainError> {
        let reported: HashSet<&FactId> = reports.iter().map(|r| &r.id).collect();
        let mut missing: Vec<FactId> = batch
            .facts
            .iter()
            .filter(|id| !reported.contains(id))
            .cloned()
            .collect();
        missing.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        missing.dedup();
        if !missing.is_empty() {
            return Err(DrainError::IncompleteOutcomes { missing });
        }
        self.write_cursor(DrainCursor {
            settled: batch.through,
            in_flight: None,
        })
        .await?;
        Ok(reports)
    }

    async fn read_ledger(&self) -> Result<Vec<LedgerEntry>, DrainError> {
        let ledger = self.ledger.clone();
        blocking(move || ledger.entries().map_err(DrainError::Ledger)).await
    }

    async fn read_cursor(&self) -> Result<DrainCursor, DrainError> {
        let path = self.cursor_path.clone();
        blocking(move || read_cursor_at(&path)).await
    }

    async fn write_cursor(&self, cursor: DrainCursor) -> Result<(), DrainError> {
        let path = self.cursor_path.clone();
        blocking(move || write_cursor_at(&path, &cursor)).await
    }
}

/// Takes the exclusive advisory lock on `<cursor_path>.lock`.
///
/// A cursor file has a single owner: two callers draining (or flushing) the
/// same cursor concurrently would race its writes, corrupting the settle
/// point or submitting the same batch twice - precisely what this crate's
/// exactly-once guarantee exists to prevent. `Ok(None)` means another live
/// drain already holds it and the caller must not proceed. The returned file
/// must outlive the drain: dropping it releases the lock.
///
/// # Errors
///
/// Any I/O failure other than the lock being held (e.g. the lock file's
/// parent directory cannot be created, or the lock file cannot be opened).
pub fn claim_sole_drain(cursor_path: &Path) -> std::io::Result<Option<File>> {
    let mut os = cursor_path.as_os_str().to_owned();
    os.push(".lock");
    let path = PathBuf::from(os);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Runs blocking filesystem work off the async runtime's worker threads.
async fn blocking<T, F>(work: F) -> Result<T, DrainError>
where
    F: FnOnce() -> Result<T, DrainError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(e) => Err(DrainError::Worker(e.to_string())),
    }
}

/// Refuses to run when the cursor points past the ledger's end - the ledger
/// was truncated or rewritten, and resuming would re-submit settled facts.
fn guard_cursor(cursor: usize, entries: usize) -> Result<(), DrainError> {
    if cursor > entries {
        return Err(DrainError::CursorAheadOfLedger { cursor, entries });
    }
    Ok(())
}

/// The next up-to-`limit` facts at or after entry `from`, and the entry count
/// they account for (one past the last entry inspected).
fn next_batch(
    entries: &[LedgerEntry],
    from: usize,
    limit: usize,
) -> (Vec<PendingFactRecord>, usize) {
    let mut facts = Vec::new();
    let mut through = from;
    for (idx, entry) in entries.iter().enumerate().skip(from) {
        through = idx + 1;
        if let LedgerEntry::PendingFact(fact) = entry {
            facts.push(fact.clone());
            if facts.len() >= limit {
                break;
            }
        }
    }
    (facts, through)
}

/// An absent cursor file is a drain that has never run, not an error.
fn read_cursor_at(path: &Path) -> Result<DrainCursor, DrainError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| DrainError::CursorEncoding {
            path: path.to_path_buf(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DrainCursor::default()),
        Err(source) => Err(DrainError::Cursor {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Writes the cursor via a synced temporary file and a rename, so a crash
/// mid-write leaves the previous cursor intact rather than a truncated one.
fn write_cursor_at(path: &Path, cursor: &DrainCursor) -> Result<(), DrainError> {
    let io = |source| DrainError::Cursor {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
    }
    let encoded = serde_json::to_vec(cursor).map_err(|source| DrainError::CursorEncoding {
        path: path.to_path_buf(),
        source,
    })?;
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    {
        let mut file = std::fs::File::create(&tmp).map_err(io)?;
        file.write_all(&encoded).map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(io)?;
    // Best effort: on unix this makes the rename itself durable, so a power
    // loss cannot resurrect the previous cursor. Platforms that refuse to open
    // a directory as a file (Windows) simply skip it - see the module docs for
    // what that costs.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::provenance::{ContentDigest, FactSource};

    use crate::ledger::DocumentRecord;

    fn fact(id: &str) -> LedgerEntry {
        LedgerEntry::PendingFact(PendingFactRecord {
            id: FactId::new(id),
            fact: format!("Fact {id}."),
            probe: None,
            source: FactSource::UserStated,
            recorded_at: 1,
        })
    }

    fn document() -> LedgerEntry {
        LedgerEntry::Document(DocumentRecord {
            digest: ContentDigest::from_hex("abc123"),
            uri: "file:///spec.md".into(),
            ingested_at: 1,
        })
    }

    #[test]
    fn a_batch_stops_at_the_limit_and_reports_the_entries_it_consumed() {
        let entries = vec![document(), fact("a"), fact("b"), fact("c")];

        let (facts, through) = next_batch(&entries, 0, 2);
        assert_eq!(facts.len(), 2);
        assert_eq!(through, 3, "one past the second fact, not past the third");

        // Resuming from there picks up exactly the remainder.
        let (facts, through) = next_batch(&entries, through, 2);
        assert_eq!(facts.len(), 1);
        assert_eq!(through, 4);

        // And a drained ledger yields nothing without moving the cursor.
        assert_eq!(next_batch(&entries, through, 2), (Vec::new(), 4));
    }

    #[test]
    fn document_only_entries_advance_the_cursor_without_a_submission() {
        let entries = vec![document(), document()];
        assert_eq!(next_batch(&entries, 0, 8), (Vec::new(), 2));
    }

    #[test]
    fn a_cursor_round_trips_through_a_crash_safe_write() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nested/cursor.json");

        assert_eq!(
            read_cursor_at(&path).expect("absent cursor"),
            DrainCursor::default(),
            "a drain that never ran is at the start, not an error"
        );

        let cursor = DrainCursor {
            settled: 3,
            in_flight: Some(InFlightBatch {
                through: 5,
                facts: vec![FactId::new("f-1")],
            }),
        };
        write_cursor_at(&path, &cursor).expect("write cursor");
        assert_eq!(read_cursor_at(&path).expect("read cursor"), cursor);
    }

    #[test]
    fn a_cursor_past_the_end_of_the_ledger_is_refused_not_replayed() {
        assert!(matches!(
            guard_cursor(4, 2),
            Err(DrainError::CursorAheadOfLedger {
                cursor: 4,
                entries: 2
            })
        ));
        assert!(guard_cursor(2, 2).is_ok(), "a fully drained ledger is fine");
    }

    #[test]
    fn a_second_claim_over_the_same_cursor_is_refused_while_the_first_holds_it() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cursor_path = dir.path().join("pending-facts.jsonl.cursor.json");

        let first = claim_sole_drain(&cursor_path)
            .expect("lock io")
            .expect("an unheld lock is claimable");
        assert!(
            claim_sole_drain(&cursor_path)
                .expect("lock io")
                .is_none(),
            "a second, concurrent claimant must not also acquire the lock"
        );

        drop(first);
        assert!(
            claim_sole_drain(&cursor_path).expect("lock io").is_some(),
            "the lock is claimable again once the holder releases it"
        );
    }
}
