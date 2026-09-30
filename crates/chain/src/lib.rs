// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Hash-chained append-only JSONL log.
//!
//! Each line is a [`ChainedLine`]:
//!
//! ```json
//! {"prev_hash":"<hex>","hash":"<hex>","entry":{...}}
//! ```
//!
//! where `hash = sha256(prev_hash || canonical_json(entry))` (both as UTF-8
//! bytes, hash hex-encoded). The first line of a file chains from
//! [`GENESIS_HASH`]. `entry` is caller-defined JSON — this crate has no
//! opinion on its shape; `sven_executors::audit::AuditExecutor` wraps
//! `{"kind": ..., "timestamp": ..., "record": {...}}`, and
//! A ledger built on top of it wraps its own entry shape.
//!
//! # Integrity guarantees — and their limits
//!
//! The chain detects **accidental corruption** and **non-adaptive
//! tampering**: editing, reordering, inserting, or deleting a line breaks
//! the chain (which [`verify_chain`] reports) *unless every subsequent hash
//! is recomputed*. The chain is **not** tamper-evident against an adversary
//! with write access to the file: there is no secret key (HMAC) and no
//! external anchor of the tip hash, so anyone who can edit the file can
//! alter or delete any entry and re-derive all subsequent hashes from
//! [`GENESIS_HASH`] in milliseconds, after which [`verify_chain`] passes.
//! Suffix truncation is likewise undetectable. Do not build security claims
//! on this chain alone; genuine tamper-evidence requires a keyed MAC,
//! OS-level append-only storage, or periodic external anchoring of the tip
//! hash, none of which is implemented.
//!
//! # Concurrency and crash safety
//!
//! Several writers may legitimately share one log file. Every
//! [`append_chain`] call therefore takes an **exclusive advisory lock** on a
//! `<log>.lock` sidecar file, re-reads the chain tip from disk, and appends
//! the whole batch in a single write — concurrent writers serialize and
//! always chain from the true tip instead of a stale in-memory one. A torn
//! trailing line (crash or disk-full mid-write) is truncated away on the
//! next [`append_chain`] call before appending. [`read_chain`] takes a
//! **shared** advisory lock so a concurrent append is never observed
//! half-written, and tolerates an unterminated final line (a live crash-torn
//! tail) without reporting it as tampering.
//!
//! # Legacy (pre-chain) files
//!
//! A log written before a chain format existed would make [`verify_chain`]
//! fail at line 1 forever, including for newly appended chained records.
//! [`append_chain`] rotates such a file aside to `<log>.legacy-<timestamp>`
//! and starts a fresh chain, so new records remain verifiable in place.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod tip;

use tip::prepare_chain_tip;

/// The `prev_hash` of the first line in a fresh chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One line of the hash-chained log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainedLine {
    /// Hash of the previous line ([`GENESIS_HASH`] for the first line).
    pub prev_hash: String,
    /// `sha256(prev_hash || canonical_json(entry))`, hex-encoded.
    pub hash: String,
    /// The caller-defined entry payload.
    pub entry: serde_json::Value,
}

/// Why [`verify_chain`] (or [`read_chain`]) rejected a log.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// The file could not be read.
    #[error("cannot read chain: {0}")]
    Io(#[from] std::io::Error),
    /// A line is not a valid [`ChainedLine`].
    #[error("line {line}: malformed entry: {reason}")]
    Malformed {
        /// 1-based line number.
        line: usize,
        /// Parse failure detail.
        reason: String,
    },
    /// A line's `prev_hash` does not match the preceding line's `hash`
    /// (line inserted, removed, or reordered).
    #[error("line {line}: broken chain: prev_hash {found} != expected {expected}")]
    BrokenLink {
        /// 1-based line number.
        line: usize,
        /// The preceding line's hash.
        expected: String,
        /// The `prev_hash` actually found.
        found: String,
    },
    /// A line's recorded `hash` does not match its recomputed hash
    /// (entry content was altered).
    #[error("line {line}: hash mismatch: entry content was altered")]
    HashMismatch {
        /// 1-based line number.
        line: usize,
    },
}

/// Hex-encoded `sha256(prev_hash || entry_json)`.
fn chain_hash(prev_hash: &str, entry_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(entry_json.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// The canonical serialization an entry is hashed over: compact JSON with
/// object keys emitted in ascending byte-wise order at every nesting level.
///
/// Sorting is enforced explicitly rather than relying on
/// [`serde_json::to_string`], whose key order flips to insertion order if
/// any crate in the build tree enables `serde_json`'s `preserve_order`
/// feature. A chain outlives the binary that wrote it; a build-feature
/// change must not make untouched historical entries fail verification.
/// (Under the default sorted-`BTreeMap` feature this produces byte-identical
/// output to `serde_json::to_string`, so logs written by older builds still
/// verify.)
pub fn canonical_json(entry: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(entry, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(key).expect("string serialization cannot fail"),
                );
                out.push(':');
                write_canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => {
            out.push_str(&serde_json::to_string(scalar).expect("scalar serialization cannot fail"))
        }
    }
}

/// Validates the hash chain of the log at `path`.
///
/// Returns the number of verified entries. An empty or absent chain is not an
/// error: a missing file verifies as 0 entries.
///
/// Note the limits of what "verified" means here — see the module docs: an
/// adversary with write access can rewrite the entire chain consistently, so
/// a passing verification only rules out accidental corruption and
/// tampering by actors who could not rewrite the file.
///
/// # Errors
///
/// Returns a [`ChainError`] pinpointing the first line at which the chain is
/// malformed, broken (insertion/removal/reordering), or altered (content
/// tampering).
pub fn verify_chain(path: impl AsRef<Path>) -> Result<usize, ChainError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(0);
    }
    let content = read_locked(path)?;
    Ok(verify_content(&content)?.len())
}

/// Reads and verifies the whole chain at `path`, returning its lines in
/// append order.
///
/// Unlike [`verify_chain`] this is meant for *live readers* that share the
/// log with concurrent writers:
///
/// * the read happens under a **shared advisory lock** on the same
///   `<path>.lock` sidecar [`append_chain`] holds exclusively, so a batch
///   being appended can never be observed half-written;
/// * a crash-torn, **unterminated** final line (crash or disk-full
///   mid-append) is ignored rather than reported as tampering — it is
///   exactly what [`append_chain`] truncates on the next write, so readers
///   and the writer agree on the chain's content. `append_chain` terminates
///   every line with `\n`, so a missing final terminator can only be a torn
///   tail, never a valid entry.
///
/// A missing file yields an empty chain.
///
/// # Errors
///
/// Same as [`verify_chain`] for everything except the torn-tail case above.
pub fn read_chain(path: impl AsRef<Path>) -> Result<Vec<ChainedLine>, ChainError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = read_locked(path)?;
    let complete = if content.ends_with('\n') || content.is_empty() {
        content.as_str()
    } else {
        let keep = content.rfind('\n').map_or(0, |i| i + 1);
        tracing::warn!(
            path = %path.display(),
            ignored_bytes = content.len() - keep,
            "read_chain: ignoring torn, unterminated final line (crash mid-append); \
             the next append will truncate it"
        );
        &content[..keep]
    };
    verify_content(complete)
}

/// Reads the log under a shared advisory lock on the `<path>.lock` sidecar,
/// so a concurrent [`append_chain`] (which holds the lock exclusively for
/// the whole read-tip-then-write cycle) is never observed mid-write.
///
/// When the sidecar cannot be opened (e.g. read-only media), falls back to
/// an unlocked read so offline verification still works.
fn read_locked(path: &Path) -> std::io::Result<String> {
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path_for(path));
    let _lock_guard = match lock_file {
        Ok(file) => {
            file.lock_shared()?;
            Some(file)
        }
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "chain read: cannot open lock sidecar; reading unlocked"
            );
            None
        }
    };
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        // The file vanished between the caller's existence check and the
        // locked read: an empty chain.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Verifies `content` (full log text) as a hash chain, returning the parsed
/// lines in order. See [`verify_chain`] for the error semantics.
fn verify_content(content: &str) -> Result<Vec<ChainedLine>, ChainError> {
    let mut expected_prev = GENESIS_HASH.to_string();
    let mut lines = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = idx + 1;
        let parsed: ChainedLine =
            serde_json::from_str(line).map_err(|e| ChainError::Malformed {
                line: line_no,
                reason: e.to_string(),
            })?;
        if parsed.prev_hash != expected_prev {
            return Err(ChainError::BrokenLink {
                line: line_no,
                expected: expected_prev,
                found: parsed.prev_hash,
            });
        }
        let recomputed = chain_hash(&parsed.prev_hash, &canonical_json(&parsed.entry));
        if recomputed != parsed.hash {
            return Err(ChainError::HashMismatch { line: line_no });
        }
        expected_prev = parsed.hash.clone();
        lines.push(parsed);
    }
    Ok(lines)
}

/// Sidecar lock-file path for `log_path` (`<log>.lock`).
///
/// The lock lives in a separate file that is never rotated or truncated, so
/// every writer — across threads and processes — locks the same inode for
/// the log's whole lifetime.
fn lock_path_for(log_path: &Path) -> PathBuf {
    let mut os = log_path.as_os_str().to_owned();
    os.push(".lock");
    PathBuf::from(os)
}

/// Appends `entries` as hash-chained JSONL lines to the log at `path`,
/// returning the number of lines written.
///
/// See the module docs for the line format, the integrity guarantees and
/// their limits, and the concurrency/crash-safety behavior.
///
/// Serializes against concurrent writers via the `<path>.lock` sidecar file
/// and always chains from the on-disk tip, so interleaved writers extend one
/// linear chain. Performs **blocking** filesystem I/O: call from a blocking
/// context (e.g. `tokio::task::spawn_blocking`) inside async code.
///
/// # Errors
///
/// Returns the underlying I/O error when the log directory cannot be
/// created, the lock cannot be acquired, or the append fails. On error
/// nothing from this batch is guaranteed to be durable; retrying never forks
/// the chain but at worst duplicates records (at-least-once). That is fine
/// for records whose duplication doesn't change meaning; callers appending
/// entries whose duplication would change meaning (e.g. monetary ledger
/// entries) must **not** blindly retry on error — see a ledger's
/// another chained writer's append path.
pub fn append_chain(path: &Path, entries: Vec<serde_json::Value>) -> std::io::Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Serialize writers (other sessions, other processes) on a sidecar lock
    // file, then chain from the *actual* on-disk tip. Never trusting a cached
    // tip means a concurrent writer or a previously failed partial flush
    // cannot fork the chain: at worst a retry duplicates records
    // (at-least-once).
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path_for(path))?;
    lock_file.lock()?;

    let mut prev = prepare_chain_tip(path)?;

    // Build the whole batch in memory and append it with a single write so a
    // mid-batch failure cannot leave interleaved partial lines from this
    // flush.
    let written = entries.len();
    let mut batch = String::new();
    for entry in entries {
        let entry_json = canonical_json(&entry);
        let hash = chain_hash(&prev, &entry_json);
        let line = ChainedLine {
            prev_hash: prev,
            hash: hash.clone(),
            entry,
        };
        batch.push_str(
            &serde_json::to_string(&line).expect("ChainedLine serialization cannot fail"),
        );
        batch.push('\n');
        prev = hash;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, batch.as_bytes())?;
    std::io::Write::flush(&mut file)?;
    // Lock released when `lock_file` drops.
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tip::TIP_WINDOW_BYTES;

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        // Build an object whose insertion order differs from sorted order so
        // the test is meaningful even if serde_json ever preserves insertion
        // order in this build.
        let mut inner = serde_json::Map::new();
        inner.insert("zeta".into(), serde_json::json!(1));
        inner.insert("alpha".into(), serde_json::json!([{"b": 2, "a": 1}]));
        let mut outer = serde_json::Map::new();
        outer.insert("outer_z".into(), serde_json::Value::Object(inner));
        outer.insert("outer_a".into(), serde_json::json!("x"));
        let value = serde_json::Value::Object(outer);

        assert_eq!(
            canonical_json(&value),
            r#"{"outer_a":"x","outer_z":{"alpha":[{"a":1,"b":2}],"zeta":1}}"#
        );
    }

    #[test]
    fn verify_chain_of_missing_file_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(verify_chain(dir.path().join("nope.jsonl")).unwrap(), 0);
    }

    #[test]
    fn append_chain_then_verify_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        let n = append_chain(
            &path,
            vec![
                serde_json::json!({"i": 1}),
                serde_json::json!({"i": 2}),
                serde_json::json!({"i": 3}),
            ],
        )
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(verify_chain(&path).unwrap(), 3);
        assert_eq!(read_chain(&path).unwrap().len(), 3);
    }

    #[test]
    fn append_chain_resumes_from_the_existing_tip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        append_chain(&path, vec![serde_json::json!({"i": 1})]).unwrap();
        append_chain(&path, vec![serde_json::json!({"i": 2})]).unwrap();
        assert_eq!(verify_chain(&path).unwrap(), 2);
    }

    #[test]
    fn verify_chain_detects_tampering() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        append_chain(
            &path,
            vec![
                serde_json::json!({"note": "Idle"}),
                serde_json::json!({"note": "Working"}),
                serde_json::json!({"note": "Done"}),
            ],
        )
        .unwrap();

        assert_eq!(verify_chain(&path).unwrap(), 3);
        let original = std::fs::read_to_string(&path).unwrap();

        // Tamper 1: alter the content of the middle record.
        let tampered = original.replacen("\"Working\"", "\"Cover-up\"", 1);
        assert_ne!(tampered, original, "tamper must change the file");
        std::fs::write(&path, &tampered).unwrap();
        assert!(
            matches!(
                verify_chain(&path),
                Err(ChainError::HashMismatch { line: 2 })
            ),
            "altered record content must be detected on the altered line"
        );

        // Tamper 2: delete the middle line (breaks the prev_hash link).
        let mut lines: Vec<&str> = original.lines().collect();
        lines.remove(1);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert!(
            matches!(
                verify_chain(&path),
                Err(ChainError::BrokenLink { line: 2, .. })
            ),
            "removed line must break the chain"
        );

        // Tamper 3: reorder lines.
        let mut lines: Vec<&str> = original.lines().collect();
        lines.swap(1, 2);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert!(
            matches!(verify_chain(&path), Err(ChainError::BrokenLink { .. })),
            "reordered lines must break the chain"
        );

        // Untampered file still verifies.
        std::fs::write(&path, &original).unwrap();
        assert_eq!(verify_chain(&path).unwrap(), 3);
    }

    #[test]
    fn read_chain_tolerates_a_torn_unterminated_tail() {
        // Live readers must not report a crash-torn tail as tampering: the
        // torn line is ignored, matching what the next append truncates.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");

        assert!(
            read_chain(&path).unwrap().is_empty(),
            "missing file reads as an empty chain"
        );

        append_chain(&path, vec![serde_json::json!({"i": 1})]).unwrap();
        assert_eq!(read_chain(&path).unwrap().len(), 1);

        // Simulate a torn write (no trailing newline).
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"prev_hash\":\"deadbeef\",\"ha").unwrap();
        }
        assert!(verify_chain(&path).is_err(), "strict verify still fails");
        assert_eq!(
            read_chain(&path).unwrap().len(),
            1,
            "read_chain ignores the torn tail"
        );

        // Real tampering (a newline-terminated bogus line) is still caught.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"ck\":\"bogus\",\"entry\":{}}\n").unwrap();
        }
        assert!(
            read_chain(&path).is_err(),
            "a terminated bogus line is tampering, not a torn tail"
        );
    }

    #[test]
    fn legacy_pre_chain_file_is_rotated_on_next_append() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        std::fs::write(&path, "not a chained line\nanother legacy line\n").unwrap();

        append_chain(&path, vec![serde_json::json!({"i": 1})]).unwrap();

        // The path now holds a fresh, single-entry, verifiable chain.
        assert_eq!(verify_chain(&path).unwrap(), 1);
        // And a sibling *.legacy-<timestamp> file holds the original content.
        let legacy: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".legacy-"))
            .collect();
        assert_eq!(
            legacy.len(),
            1,
            "exactly one legacy file should be rotated aside"
        );
    }

    /// The tip lookup reads a fixed-size tail window, so the damage it must
    /// repair can be larger than that window. It has to widen until it reaches
    /// the last good line rather than mistaking the window edge for the tip
    /// and forking the chain.
    #[test]
    fn torn_tail_larger_than_the_tip_window_is_still_repaired() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("chain.jsonl");
        append_chain(&path, vec![serde_json::json!({"i": 1})]).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();

        // Garbage well past TIP_WINDOW_BYTES, all newline-terminated so it is
        // unparseable rather than merely torn.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            let junk = format!("{}\n", "JUNK".repeat(63));
            for _ in 0..(4 * TIP_WINDOW_BYTES / 256) {
                f.write_all(junk.as_bytes()).unwrap();
            }
        }
        assert!(
            std::fs::metadata(&path).unwrap().len() > TIP_WINDOW_BYTES,
            "the junk must exceed one window for this test to mean anything"
        );

        append_chain(&path, vec![serde_json::json!({"i": 2})]).unwrap();

        // The junk is gone and the new record chains from the real tip.
        assert_eq!(verify_chain(&path).unwrap(), 2);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with(&good), "the good prefix must survive");
        assert!(!content.contains("JUNK"), "no junk may survive: {content}");
    }

    /// Appending must cost time proportional to the **batch**, not to the
    /// history already on disk.
    ///
    /// `AuditExecutor` appends on every audited effect and other writers on
    /// every billing debit, for the whole life of a long-running server. If
    /// finding the chain tip re-reads and re-parses the entire log, those
    /// paths degrade quadratically in the number of records and the resident
    /// memory of a single append grows without bound.
    ///
    /// The assertion is relative (big-file appends vs. empty-file appends) so
    /// it calibrates itself to the host instead of pinning a wall-clock
    /// budget, with a floor so timer noise on a fast machine cannot make it
    /// flaky.
    #[test]
    fn appending_does_not_rescan_existing_history() {
        use std::time::{Duration, Instant};

        const APPENDS: usize = 100;
        const HISTORY: usize = 4_000;

        // ~1 KiB per entry, so HISTORY entries make a multi-megabyte file.
        let payload = "x".repeat(1024);
        let entry = |i: usize| serde_json::json!({ "i": i, "payload": payload });

        let dir = tempfile::TempDir::new().unwrap();

        // Baseline: appends against a chain that stays near-empty.
        let small = dir.path().join("small.jsonl");
        let started = Instant::now();
        for i in 0..APPENDS {
            append_chain(&small, vec![entry(i)]).unwrap();
        }
        let small_elapsed = started.elapsed();

        // Same appends against a chain with a long history in front of them.
        let big = dir.path().join("big.jsonl");
        append_chain(&big, (0..HISTORY).map(entry).collect()).unwrap();
        let started = Instant::now();
        for i in 0..APPENDS {
            append_chain(&big, vec![entry(i)]).unwrap();
        }
        let big_elapsed = started.elapsed();

        assert_eq!(verify_chain(&big).unwrap(), HISTORY + APPENDS);

        let budget = std::cmp::max(small_elapsed * 8, Duration::from_millis(400));
        assert!(
            big_elapsed <= budget,
            "appending behind {HISTORY} records took {big_elapsed:?}, but the same \
             {APPENDS} appends on an empty chain took {small_elapsed:?} (budget \
             {budget:?}) — the tip lookup is rescanning the whole log"
        );
    }
}
