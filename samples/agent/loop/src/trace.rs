// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents whose audit
// trails are complete enough to review after a restart. If your team needs
// expertise in agent observability or audit evidence, you can procure our
// services by sending an email to info@swedishembedded.com.

//! The append-only trace: one JSON object per line, nothing ever edited.
//!
//! The trace is what makes a run reviewable after the fact - every tool call,
//! every usage report and every limit that fired, in order, with a stable id.
//! A final narrative claim ("all tests pass") is not reviewable evidence;
//! this file is.
//!
//! Events are schema-versioned (`v: 1`) and carry the run id, the attempt
//! number, a monotonically increasing sequence number within the run, and a
//! UTC timestamp. Large payloads are NOT truncated silently: the full text
//! goes to `artifacts/` under the run directory and the event carries a
//! reference, its size and its hash, so a dropped event is visible instead
//! of invisible.

use anyhow::Context;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema version of the event stream.
pub const SCHEMA_VERSION: u32 = 1;

/// A single string longer than this is bound to an artifact file instead of
/// inlined into the event line.
const INLINE_LIMIT_BYTES: usize = 8 * 1024;

/// The append-only writer for one run. Sequence numbers are assigned here,
/// under a lock, so concurrent emitters never reorder.
pub struct Trace {
    run_id: String,
    attempt: u32,
    file: Mutex<TraceFile>,
    artifacts: PathBuf,
    artifact_seq: Mutex<u64>,
}

struct TraceFile {
    file: File,
    seq: u64,
}

impl Trace {
    /// Opens (or creates) `events.jsonl` in `run_dir`. An existing file is
    /// appended to: a resumed run continues its trace, it never starts a
    /// second one, because a second file would break the property that
    /// makes the trace reviewable - that it is the complete record.
    pub fn open(run_dir: &Path, run_id: &str, attempt: u32) -> anyhow::Result<Self> {
        std::fs::create_dir_all(run_dir)?;
        let artifacts = run_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let path = run_dir.join("events.jsonl");
        // A resumed run reopens this file, so the sequence continues where
        // the previous attempt left off - a second attempt re-emitting seq 1
        // would make the file unreadable by its own monotonicity rule.
        let seq = last_sequence(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            run_id: run_id.to_string(),
            attempt,
            file: Mutex::new(TraceFile { file, seq }),
            artifacts,
            artifact_seq: Mutex::new(0),
        })
    }

    /// Appends one event with `kind` and `payload`. Every string in the
    /// payload longer than the inline limit is replaced in place by a
    /// reference to an artifact file carrying the full text. Returns how
    /// many artifacts were bound.
    ///
    /// Each event is fsynced before the call returns, so a crash can cost
    /// the last event only.
    pub fn event(&self, kind: &str, payload: &mut serde_json::Value) -> anyhow::Result<usize> {
        let bound = self.bind_large_strings(payload)?;
        let mut guard = self.file.lock().unwrap();
        guard.seq += 1;
        let event = Event {
            v: SCHEMA_VERSION,
            run: &self.run_id,
            attempt: self.attempt,
            seq: guard.seq,
            ts: crate::clock::utc_now(),
            kind,
            payload: payload.clone(),
        };
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');
        guard.file.write_all(line.as_bytes())?;
        guard.file.flush()?;
        guard.file.sync_all()?;
        Ok(bound)
    }

    fn bind_large_strings(&self, payload: &mut serde_json::Value) -> anyhow::Result<usize> {
        let mut count = 0usize;
        let mut seq = self.artifact_seq.lock().unwrap();
        bind_in_place(payload, &self.artifacts, &mut seq, &mut count)?;
        Ok(count)
    }
}

/// A single trace event, as it is written to disk.
#[derive(Clone, Debug, serde::Serialize)]
struct Event<'a> {
    v: u32,
    run: &'a str,
    attempt: u32,
    seq: u64,
    /// UTC, `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    ts: String,
    #[serde(rename = "type")]
    kind: &'a str,
    payload: serde_json::Value,
}

fn bind_in_place(
    value: &mut serde_json::Value,
    artifacts: &Path,
    seq: &mut u64,
    count: &mut usize,
) -> anyhow::Result<()> {
    match value {
        serde_json::Value::String(text) if text.len() > INLINE_LIMIT_BYTES => {
            *value = bind_string(text, artifacts, "bound", seq)?;
            *count += 1;
        }
        serde_json::Value::Array(items) => {
            for item in items {
                bind_in_place(item, artifacts, seq, count)?;
            }
        }
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let Some(item) = map.get_mut(&key) else {
                    continue;
                };
                if let serde_json::Value::String(text) = item {
                    if text.len() > INLINE_LIMIT_BYTES {
                        *item = bind_string(text, artifacts, &key, seq)?;
                        *count += 1;
                        continue;
                    }
                }
                bind_in_place(item, artifacts, seq, count)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Writes the full text to `artifacts/` and returns the reference marker the
/// event line carries instead. The marker names the file, its size and its
/// sha256, so nothing in the trace looks complete when it is a pointer -
/// and a reviewer can verify the pointer against the file it names.
fn bind_string(
    text: &str,
    artifacts: &Path,
    key: &str,
    seq: &mut u64,
) -> anyhow::Result<serde_json::Value> {
    *seq += 1;
    let hash = hex(&Sha256::digest(text.as_bytes()));
    let name = format!("a{seq:04}-{key}.txt");
    let path = artifacts.join(&name);
    std::fs::create_dir_all(artifacts)?;
    std::fs::write(&path, text)?;
    Ok(serde_json::json!({
        "bound": path.display().to_string(),
        "size": text.len(),
        "hash": hash,
    }))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The highest sequence number already in the file, or 0 for a fresh one. A
/// torn trailing line (a crash between write and fsync) is skipped: it has
/// no complete event behind it, and the next append starts above the last
/// whole line.
fn last_sequence(path: &Path) -> anyhow::Result<u64> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut seq = 0u64;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        // Torn line: parse failure means the process died mid-write. The
        // truncation is left to the reader (which refuses the line); here it
        // only must not stop the sequence from continuing past the intact
        // prefix.
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            break;
        };
        seq = seq.max(event.get("seq").and_then(|v| v.as_u64()).unwrap_or(0));
    }
    Ok(seq)
}

/// Reads a trace back, in order, refusing a file whose sequence is not
/// monotone - a reordered or partially-written trace must never be read as
/// evidence.
pub fn read_events(run_dir: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    let path = run_dir.join("events.jsonl");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut events = Vec::new();
    let mut seq = 0u64;
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("{} line {}: not JSON", path.display(), n + 1))?;
        let this = event.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        anyhow::ensure!(
            this > seq,
            "{} line {}: seq {this} is not monotone past {seq}",
            path.display(),
            n + 1
        );
        seq = this;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_monotone_and_a_large_output_is_bound_not_dropped() {
        let dir = std::env::temp_dir().join(format!("loop-trace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let trace = Trace::open(&dir, "loop-test", 1).unwrap();

        let mut first = serde_json::json!({ "model": "brain/qwen3" });
        trace.event("run_started", &mut first).unwrap();

        let big = "x".repeat(INLINE_LIMIT_BYTES + 1);
        let mut second = serde_json::json!({ "output": big, "is_error": false });
        let bound = trace.event("tool_finished", &mut second).unwrap();
        assert_eq!(
            bound, 1,
            "a payload over the limit must be bound to an artifact"
        );

        let marker = second.get("output").unwrap();
        let path = marker.get("bound").and_then(|v| v.as_str()).unwrap();
        assert!(Path::new(path).exists(), "the artifact file must exist");
        assert!(
            marker.get("hash").and_then(|v| v.as_str()).is_some(),
            "the marker must carry the hash a reviewer verifies against"
        );
        let events = read_events(&dir).unwrap();
        assert_eq!(events.len(), 2);
        let seqs: Vec<u64> = events
            .iter()
            .filter_map(|e| e.get("seq").and_then(|v| v.as_u64()))
            .collect();
        assert_eq!(seqs, vec![1, 2], "seq must be monotone");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_non_monotone_trace_is_refused_not_read_as_evidence() {
        let dir = std::env::temp_dir().join(format!("loop-trace-holes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":2}\n{\"seq\":1}\n").unwrap();
        assert!(
            read_events(&dir).is_err(),
            "a reordered trace must be refused"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_trace_continues_its_sequence_when_the_same_file_is_reopened() {
        let dir = std::env::temp_dir().join(format!("loop-trace-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let trace = Trace::open(&dir, "loop-test", 1).unwrap();
            let mut payload = serde_json::json!({});
            trace.event("one", &mut payload).unwrap();
        }
        {
            // A resumed run reopens the same file - attempt 2 continues the
            // sequence rather than starting a second history.
            let trace = Trace::open(&dir, "loop-test", 2).unwrap();
            let mut payload = serde_json::json!({});
            trace.event("two", &mut payload).unwrap();
        }
        let events = read_events(&dir).unwrap();
        assert_eq!(events.len(), 2, "one file, not one per attempt");
        assert_eq!(events[1].get("attempt").and_then(|v| v.as_u64()), Some(2));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
