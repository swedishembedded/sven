// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Finding the chain tip to append from, and repairing a damaged tail.
//!
//! Split out of `lib.rs` because it is a self-contained concern with its own
//! invariants: it must decide where a new line chains from, and how much of a
//! crash-torn or legacy file to cut away, while reading only a **bounded**
//! slice of the log. Everything here is called by `append_chain` under the
//! log's exclusive advisory lock.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::{ChainedLine, GENESIS_HASH};

/// Moves a legacy (pre-chain) log file aside to `<log>.legacy-<timestamp>`
/// so a fresh, verifiable chain can be started at `path`.
fn rotate_legacy(path: &Path) -> std::io::Result<()> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
    let mut dest = PathBuf::from({
        let mut os = path.as_os_str().to_owned();
        os.push(format!(".legacy-{stamp}"));
        os
    });
    let mut attempt = 1u32;
    while dest.exists() {
        let mut os = path.as_os_str().to_owned();
        os.push(format!(".legacy-{stamp}-{attempt}"));
        dest = PathBuf::from(os);
        attempt += 1;
    }
    std::fs::rename(path, &dest)?;
    tracing::info!(
        from = %path.display(),
        to = %dest.display(),
        "chain: rotated legacy (pre-chain) log; starting a fresh chain"
    );
    Ok(())
}

/// How much of the log's tail to read when looking for the chain tip. Large
/// enough that a single read covers the last complete line for any realistic
/// entry size; grown geometrically on the rare occasion that it does not.
pub(crate) const TIP_WINDOW_BYTES: u64 = 64 * 1024;

/// How far the legacy-format probe reads looking for the first complete line.
/// A file whose first line is longer than this is treated as **not** legacy:
/// rotating a file aside is destructive, so an inconclusive read must never
/// trigger it.
const LEGACY_PROBE_BYTES: u64 = 1024 * 1024;

/// Determines the hash the next appended line must chain from, repairing the
/// file tail if needed. **Must be called with the log's advisory lock held.**
///
/// * Missing or empty file → [`GENESIS_HASH`].
/// * First line is not a [`ChainedLine`] → legacy pre-chain file: rotate it
///   aside (see [`rotate_legacy`]) and start from [`GENESIS_HASH`].
/// * Otherwise resume from the hash of the last parseable, newline-terminated
///   line. Anything after it (a torn line from a crash or disk-full
///   mid-write) is truncated so the chain stays verifiable.
///
/// Both probes read a **bounded** slice of the file — a prefix for the legacy
/// check, a tail window for the tip — so an append costs time proportional to
/// its own batch rather than to the history already on disk. `AuditExecutor`
/// and `CreditLedger` append for the whole life of a server; re-reading the
/// entire log per append made those paths quadratic in record count and gave
/// a single append unbounded resident memory.
pub(crate) fn prepare_chain_tip(path: &Path) -> std::io::Result<String> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GENESIS_HASH.to_string()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(GENESIS_HASH.to_string());
    }

    match probe_first_line(&mut file, len)? {
        // Whitespace only: an empty chain, with nothing worth rotating.
        FirstLine::Blank => return Ok(GENESIS_HASH.to_string()),
        FirstLine::Legacy => {
            rotate_legacy(path)?;
            return Ok(GENESIS_HASH.to_string());
        }
        FirstLine::Chained | FirstLine::Inconclusive => {}
    }

    let (tip, keep_bytes) = scan_tip(&mut file, len)?;

    if keep_bytes < len {
        tracing::warn!(
            path = %path.display(),
            truncated_bytes = len - keep_bytes,
            "chain: truncating torn/unparsable tail of log before appending"
        );
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.set_len(keep_bytes)?;
    }
    Ok(tip)
}

/// Decodes `buf` as UTF-8, stopping at the first invalid byte. The tail window
/// is cut at a line boundary and the legacy probe at a fixed byte count, so
/// only the latter can split a multi-byte character — and only in its
/// discarded remainder.
fn decode_lossless_prefix(buf: &[u8]) -> &str {
    std::str::from_utf8(buf).unwrap_or_else(|e| {
        // `valid_up_to()` is by definition the length of a valid prefix.
        std::str::from_utf8(&buf[..e.valid_up_to()]).unwrap_or("")
    })
}

/// What the bounded prefix probe concluded about a log's format.
enum FirstLine {
    /// The first non-blank line parses as a [`ChainedLine`].
    Chained,
    /// The first non-blank line does not parse: a pre-chain legacy file.
    Legacy,
    /// The file holds no non-blank line at all: an empty chain.
    Blank,
    /// The probe ran out before the first line ended. Deliberately **not**
    /// treated as legacy — rotating a file aside is destructive, so an
    /// inconclusive read must never trigger it.
    Inconclusive,
}

/// Classifies the log's first non-blank line, reading only a bounded prefix.
fn probe_first_line(file: &mut std::fs::File, len: u64) -> std::io::Result<FirstLine> {
    use std::io::{Read as _, Seek as _};

    file.seek(std::io::SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    file.take(LEGACY_PROBE_BYTES).read_to_end(&mut buf)?;
    let whole_file = buf.len() as u64 >= len;

    for segment in decode_lossless_prefix(&buf).split_inclusive('\n') {
        // An unterminated line is the real first line only if the probe read
        // to EOF; otherwise it is just where the probe was cut off.
        if !segment.ends_with('\n') && !whole_file {
            return Ok(FirstLine::Inconclusive);
        }
        let line = segment.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        return Ok(if serde_json::from_str::<ChainedLine>(line).is_ok() {
            FirstLine::Chained
        } else {
            FirstLine::Legacy
        });
    }
    Ok(if whole_file {
        FirstLine::Blank
    } else {
        FirstLine::Inconclusive
    })
}

/// Finds the tip hash and the byte offset just past the last complete,
/// parseable line, reading only the log's tail.
///
/// The window opens at a line boundary (its leading partial line is dropped)
/// and doubles out until it contains a parseable line or covers the whole
/// file, so the result matches a full forward scan.
fn scan_tip(file: &mut std::fs::File, len: u64) -> std::io::Result<(String, u64)> {
    use std::io::{Read as _, Seek as _};

    let mut window = TIP_WINDOW_BYTES;
    loop {
        let start = len.saturating_sub(window);
        file.seek(std::io::SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.take(len - start).read_to_end(&mut buf)?;

        // Only whole lines can be judged. Unless the window already reaches
        // the start of the file, discard the partial line it opens with.
        let (skip, base) = if start == 0 {
            (0usize, 0u64)
        } else if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            (i + 1, start + i as u64 + 1)
        } else {
            // No line boundary in the window at all — `start != 0` implies
            // the file is longer, so widening can still find one.
            window = window.saturating_mul(4);
            continue;
        };

        let mut tip = None;
        let mut keep_bytes = base;
        let mut offset = base;
        for segment in decode_lossless_prefix(&buf[skip..]).split_inclusive('\n') {
            let terminated = segment.ends_with('\n');
            let line = segment.trim_end_matches(['\n', '\r']);
            if line.trim().is_empty() {
                // Benign blank line: keep it when it directly extends the
                // kept region (verify_chain skips blank lines).
                if terminated && keep_bytes == offset {
                    keep_bytes = offset + segment.len() as u64;
                }
            } else if terminated {
                if let Ok(parsed) = serde_json::from_str::<ChainedLine>(line) {
                    tip = Some(parsed.hash);
                    keep_bytes = offset + segment.len() as u64;
                }
            }
            offset += segment.len() as u64;
        }

        match tip {
            Some(tip) => return Ok((tip, keep_bytes)),
            None if start == 0 => return Ok((GENESIS_HASH.to_string(), keep_bytes)),
            // The tail held only torn or unparseable lines; widen to reach
            // the last good one.
            None => window = window.saturating_mul(4),
        }
    }
}
