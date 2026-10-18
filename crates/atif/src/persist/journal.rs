// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements crash-robust agent telemetry for its clients.
// If your team needs expertise in keeping agent run records intact when
// machines fail then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Append-only trajectory journal: one record per line, written as the run
//! happens.
//!
//! The whole-document writers rewrite the file on every save, so a process
//! killed in the middle leaves either nothing new or, without care, a
//! half-written document. A journal is only ever appended to, one complete
//! line per record, written with a single `write` straight to the file (no
//! userspace buffer), so a killed process leaves every record that was
//! finished. Power loss is a separate failure the [`FlushPolicy`] decides how
//! much of to survive.
//!
//! Records, one JSON object per line, in this order:
//!
//! 1. `{"record":"header","trajectory":{..}}` - the trajectory without steps;
//! 2. `{"record":"step","step":{..}}` - zero or more;
//! 3. `{"record":"end","final_metrics":{..}|null}` - written by
//!    [`JournalWriter::finish`] and only then.
//!
//! A journal without the end record belongs to a run that did not finish
//! (crashed, killed, or still running). [`read_journal`] returns what it
//! holds with [`CAPTURE_STATUS_KEY`] set to `"incomplete"` in the trajectory's
//! `extra`, so a consumer that admits only finished runs can refuse it by
//! reading the trajectory alone.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::model::{FinalMetrics, TraceStep, Trajectory};

/// Key in a trajectory's root `extra` that says whether the capture ended
/// cleanly: `"complete"` or `"incomplete"`. Absent on a trajectory that was
/// not read from a journal.
pub const CAPTURE_STATUS_KEY: &str = "capture_status";

/// How much of what was appended survives a power loss. A process crash loses
/// nothing under any policy: every record is handed to the kernel when it is
/// appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushPolicy {
    /// `fsync` after every record: at most the record being written is lost.
    EveryRecord,
    /// `fsync` after every `n` records (`n` of 0 counts as 1): at most the
    /// last `n - 1` finished records are lost.
    EveryRecords(u32),
    /// `fsync` only when the journal is finished.
    OnClose,
}

/// Bounds a journal keeps, so one runaway record or run cannot fill a disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalLimits {
    /// The most bytes one record's line may take.
    pub max_record_bytes: usize,
    /// The most bytes the journal may hold before the end record.
    pub max_file_bytes: u64,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: 32 << 20,
            max_file_bytes: 4 << 30,
        }
    }
}

/// Errors from the journal writer and reader.
#[derive(Debug, Error)]
pub enum JournalError {
    /// Underlying I/O failure (including a journal that already exists).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A record failed to serialize.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    /// A record is larger than [`JournalLimits::max_record_bytes`]; it was not
    /// written.
    #[error("record of {bytes} bytes exceeds the {limit} byte record limit")]
    RecordTooLarge {
        /// The record's size.
        bytes: usize,
        /// The limit it broke.
        limit: usize,
    },
    /// Appending would pass [`JournalLimits::max_file_bytes`]; it was not
    /// written.
    #[error(
        "the journal holds {written} bytes; one more record would pass the {limit} byte limit"
    )]
    FileFull {
        /// Bytes already in the journal.
        written: u64,
        /// The limit.
        limit: u64,
    },
    /// A record other than the last is not valid: the journal was damaged, not
    /// merely cut short.
    #[error("line {line}: {reason}")]
    Corrupt {
        /// 1-indexed line of the bad record.
        line: usize,
        /// What is wrong with it.
        reason: String,
    },
    /// The journal has no header record.
    #[error("the journal has no header record")]
    MissingHeader,
}

#[derive(Serialize)]
#[serde(tag = "record", rename_all = "lowercase")]
enum RecordOut<'a> {
    Header {
        trajectory: &'a Trajectory,
    },
    Step {
        step: &'a TraceStep,
    },
    End {
        final_metrics: Option<&'a FinalMetrics>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
enum RecordIn {
    Header { trajectory: Trajectory },
    Step { step: TraceStep },
    End { final_metrics: Option<FinalMetrics> },
}

/// Appends records to a new journal file.
pub struct JournalWriter {
    file: File,
    policy: FlushPolicy,
    limits: JournalLimits,
    written: u64,
    unsynced: u32,
}

impl JournalWriter {
    /// Creates the journal at `path`, which must not exist, and writes its
    /// header from `header`'s fields (its steps are ignored).
    ///
    /// # Errors
    ///
    /// Fails when `path` exists or cannot be created, or the header exceeds the
    /// record limit.
    pub fn create(
        path: &Path,
        header: &Trajectory,
        policy: FlushPolicy,
        limits: JournalLimits,
    ) -> Result<Self, JournalError> {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)?;
        let mut writer = Self {
            file,
            policy,
            limits,
            written: 0,
            unsynced: 0,
        };
        let mut header = header.clone();
        header.steps.clear();
        header.final_metrics = None;
        writer.append(
            &RecordOut::Header {
                trajectory: &header,
            },
            true,
        )?;
        // The header makes the file a journal: make that durable at once.
        writer.file.sync_all()?;
        writer.unsynced = 0;
        Ok(writer)
    }

    /// Appends one step as a record.
    ///
    /// # Errors
    ///
    /// [`JournalError::RecordTooLarge`] or [`JournalError::FileFull`] when a
    /// bound would be passed (nothing is written then), or an I/O error.
    pub fn append_step(&mut self, step: &TraceStep) -> Result<(), JournalError> {
        self.append(&RecordOut::Step { step }, true)
    }

    /// Writes the end record and syncs: the run finished. `final_metrics` are
    /// the run's totals, when known.
    ///
    /// # Errors
    ///
    /// An I/O or serialization error; the journal then stays incomplete.
    pub fn finish(mut self, final_metrics: Option<&FinalMetrics>) -> Result<(), JournalError> {
        self.append(&RecordOut::End { final_metrics }, false)?;
        self.file.sync_all()?;
        Ok(())
    }

    fn append(&mut self, record: &RecordOut<'_>, bounded: bool) -> Result<(), JournalError> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        if bounded {
            if line.len() > self.limits.max_record_bytes {
                return Err(JournalError::RecordTooLarge {
                    bytes: line.len(),
                    limit: self.limits.max_record_bytes,
                });
            }
            if self.written + line.len() as u64 > self.limits.max_file_bytes {
                return Err(JournalError::FileFull {
                    written: self.written,
                    limit: self.limits.max_file_bytes,
                });
            }
        }
        // One write per record: the kernel sees a whole line or, if the
        // process dies inside the call, a tail without its newline.
        self.file.write_all(&line)?;
        self.written += line.len() as u64;
        self.unsynced += 1;
        let due = match self.policy {
            FlushPolicy::EveryRecord => true,
            FlushPolicy::EveryRecords(n) => self.unsynced >= n.max(1),
            FlushPolicy::OnClose => false,
        };
        if due {
            self.file.sync_data()?;
            self.unsynced = 0;
        }
        Ok(())
    }
}

/// What [`read_journal`] recovered.
#[derive(Debug, Clone)]
pub struct JournalRead {
    /// The trajectory: the header, every complete step, and the end record's
    /// final metrics. Its root `extra[CAPTURE_STATUS_KEY]` is `"complete"` or
    /// `"incomplete"`.
    pub trajectory: Trajectory,
    /// Whether the end record was found.
    pub complete: bool,
    /// Bytes at the end of the file that were not a whole record (a write cut
    /// short by a crash), dropped.
    pub torn_tail_bytes: u64,
}

/// Reads a journal back, tolerating a record cut short at the end.
///
/// # Errors
///
/// An I/O error; [`JournalError::MissingHeader`] when the first record is not
/// the header; [`JournalError::Corrupt`] when any whole record is invalid
/// (damage, as opposed to a crash cutting the tail).
pub fn read_journal(path: &Path) -> Result<JournalRead, JournalError> {
    let bytes = std::fs::read(path)?;
    let (whole, torn) = match bytes.iter().rposition(|b| *b == b'\n') {
        Some(last_newline) => (&bytes[..=last_newline], bytes.len() - last_newline - 1),
        None => (&bytes[..0], bytes.len()),
    };
    let mut trajectory: Option<Trajectory> = None;
    let mut complete = false;
    for (index, line) in whole.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let corrupt = |reason: String| JournalError::Corrupt {
            line: index + 1,
            reason,
        };
        let record: RecordIn = serde_json::from_slice(line).map_err(|e| corrupt(e.to_string()))?;
        if complete {
            return Err(corrupt("a record follows the end record".into()));
        }
        match (record, trajectory.as_mut()) {
            (RecordIn::Header { trajectory: header }, None) => trajectory = Some(header),
            (RecordIn::Header { .. }, Some(_)) => {
                return Err(corrupt("a second header record".into()))
            }
            (_, None) => return Err(JournalError::MissingHeader),
            (RecordIn::Step { step }, Some(t)) => t.steps.push(step),
            (RecordIn::End { final_metrics }, Some(t)) => {
                t.final_metrics = final_metrics;
                complete = true;
            }
        }
    }
    let mut trajectory = trajectory.ok_or(JournalError::MissingHeader)?;
    let extra = trajectory.extra.get_or_insert_with(|| json!({}));
    if let Some(object) = extra.as_object_mut() {
        object.insert(
            CAPTURE_STATUS_KEY.into(),
            Value::from(if complete { "complete" } else { "incomplete" }),
        );
    }
    Ok(JournalRead {
        trajectory,
        complete,
        torn_tail_bytes: torn as u64,
    })
}
