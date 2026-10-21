// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! When the CLI saw each trace record.
//!
//! The machine is clock-free, so a trace record says what happened and in what
//! order, not when. The CLI, which watches a run from outside, stamps every
//! record it writes (`--trace`) or prints (`[sven:harness]`) with
//! `"ts_ms"`: the wall clock, in milliseconds since the Unix epoch, at the
//! moment it observed the record. Per-step and per-item timings come from
//! differences between stamps.
//!
//! Swedish Embedded AB implements measurable, reproducible agent workflows for
//! its clients. If your team needs expertise in timing what autonomous runs
//! actually did then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sven_sdk::SourcedRecord;

/// Milliseconds since the Unix epoch, now.
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `record` as a JSON line, with the time it was observed.
pub(super) fn stamped_line(
    record: &SourcedRecord,
    ts_ms: u64,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(record)?;
    if let Value::Object(fields) = &mut value {
        fields.insert("ts_ms".to_string(), json!(ts_ms));
    }
    serde_json::to_string(&value)
}

/// The times at which one run's trace records were observed, in order.
#[derive(Clone)]
pub(super) struct Stamps {
    /// When the run began: the time given to records this run did not
    /// observe itself (those a resumed checkpoint already carried).
    began_ms: u64,
    seen: Arc<Mutex<Vec<u64>>>,
}

impl Stamps {
    pub(super) fn begin() -> Self {
        Self {
            began_ms: now_ms(),
            seen: Arc::default(),
        }
    }

    /// Notes that a record is being observed now, and returns when.
    pub(super) fn note(&self) -> u64 {
        let now = now_ms();
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(now);
        now
    }

    /// The observation time of each of `total` records, oldest first. The
    /// records this run observed are the last ones; any before them came
    /// from the checkpoint it resumed and are stamped with the run's start.
    pub(super) fn times(&self, total: usize) -> Vec<u64> {
        let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let carried = total.saturating_sub(seen.len());
        std::iter::repeat_n(self.began_ms, carried)
            .chain(seen.iter().copied())
            .take(total)
            .collect()
    }
}
