//! Hash-chained audit log effect executor.
//!
//! Handles [`Effect::PersistAudit`] by appending audit entries to a durable
//! JSONL file on disk. Each line is a [`ChainedLine`]:
//!
//! ```json
//! {"prev_hash":"<hex>","hash":"<hex>","entry":{"kind":"dispatch","timestamp":"...","record":{...}}}
//! ```
//!
//! where `hash = sha256(prev_hash || canonical_json(entry))` (both as UTF-8
//! bytes, hash hex-encoded). The first line of a file chains from
//! [`GENESIS_HASH`].
//!
//! # Integrity guarantees — and their limits
//!
//! The chain detects **accidental corruption** and **non-adaptive
//! tampering**: editing, reordering, inserting, or deleting a line breaks
//! the chain (which [`verify_chain`] reports) *unless every subsequent hash
//! is recomputed*. The chain is **not** tamper-evident against an adversary
//! with write access to the file: there is no secret key (HMAC) and no
//! external anchor of the tip hash, so anyone who can edit the file —
//! including a compromised tenant/operator or the agent's own file tools —
//! can alter or delete any entry and re-derive all subsequent hashes from
//! [`GENESIS_HASH`] in milliseconds, after which [`verify_chain`] passes.
//! Suffix truncation is likewise undetectable. Do not build security claims
//! on this chain alone; genuine tamper-evidence requires a keyed MAC,
//! OS-level append-only storage, or periodic external anchoring of the tip
//! hash, none of which is implemented.
//!
//! # Concurrency and crash safety
//!
//! Several executors may legitimately share one log file (concurrent
//! supervisor sessions on one workspace, a TUI plus a node on the same
//! repo). Every flush therefore takes an **exclusive advisory lock** on a
//! `<log>.lock` sidecar file, re-reads the chain tip from disk, and appends
//! the whole batch in a single write — concurrent writers serialize and
//! always chain from the true tip instead of a stale in-memory one. A torn
//! trailing line (crash or disk-full mid-write) is truncated away on the
//! next flush before appending. Failed flushes are retried on the next
//! `PersistAudit`; a retry after a partial write may duplicate records
//! (at-least-once persistence) but never forks the chain.
//!
//! # Legacy (pre-chain) files
//!
//! A log written by the pre-chain format would make [`verify_chain`] fail at
//! line 1 forever — including for newly appended chained records. When a
//! flush finds a file whose first line is not a [`ChainedLine`], the file is
//! rotated to `<log>.legacy-<timestamp>` and a fresh chain is started, so
//! new records remain verifiable in place.
//!
//! # Design note
//!
//! [`Effect::PersistAudit`] carries no payload: the kernel's own
//! [`sven_hsm::Context::audit`] field is the ground-truth audit trail. The
//! kernel mirrors it (and the per-tool-call trail) into an
//! [`AuditTrailHandle`] around every dispatch and executes `PersistAudit`
//! itself after each dispatch's effects have run, so an executor built with
//! [`AuditExecutor::with_trail`] flushes the **full** [`AuditRecord`]s
//! (including principal attribution) and `ToolAuditRecord`s that accumulated
//! since the previous flush. Without a trail ([`AuditExecutor::new`]) each
//! `PersistAudit` writes a chained `checkpoint` marker entry, preserving the
//! historical behavior.
//!
//! [`AuditRecord`]: sven_hsm::AuditRecord

use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sven_hsm::{AuditTrailHandle, Effect, EffectExecutor, EventSink, ObservationSink};

/// The `prev_hash` of the first line in a fresh audit log.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One line of the hash-chained audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainedLine {
    /// Hash of the previous line ([`GENESIS_HASH`] for the first line).
    pub prev_hash: String,
    /// `sha256(prev_hash || canonical_json(entry))`, hex-encoded.
    pub hash: String,
    /// The audit entry itself: `{"kind": "dispatch"|"tool"|"checkpoint",
    /// "timestamp": <rfc3339>, "record": {...}}` (no `record` for
    /// `checkpoint`).
    pub entry: serde_json::Value,
}

/// Why [`verify_chain`] rejected an audit log.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// The file could not be read.
    #[error("cannot read audit log: {0}")]
    Io(#[from] std::io::Error),
    /// A line is not a valid [`ChainedLine`].
    #[error("line {line}: malformed audit entry: {reason}")]
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
/// feature. An audit log outlives the binary that wrote it; a build-feature
/// change must not make untouched historical entries fail verification.
/// (Under the default sorted-`BTreeMap` feature this produces byte-identical
/// output to `serde_json::to_string`, so logs written by older builds still
/// verify.)
fn canonical_json(entry: &serde_json::Value) -> String {
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

/// Validates the hash chain of the audit log at `path`.
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
        "AuditExecutor: rotated legacy (pre-chain) audit log; starting a fresh chain"
    );
    Ok(())
}

/// Appends `entries` as hash-chained JSONL lines to the audit log at `path`,
/// returning the number of lines written.
///
/// This is the reusable core of the P0.3 hash-chained audit log (see the
/// module docs for the line format, the integrity guarantees and their
/// limits, and the concurrency/crash-safety behavior). [`AuditExecutor`]
/// calls it for `Effect::PersistAudit`; other writers — such as the
/// `sven-companion` local audit copy — call it directly to persist their own
/// entries into a chain verifiable with [`verify_chain`].
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
/// for audit records; callers appending entries whose duplication changes
/// meaning (e.g. monetary ledger entries) must **not** blindly retry — see
/// `sven-metering`'s `CreditLedger::append`.
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
    file.write_all(batch.as_bytes())?;
    file.flush()?;
    // Lock released when `lock_file` drops.
    Ok(written)
}

/// Determines the hash the next appended line must chain from, repairing the
/// file tail if needed. **Must be called with the log's advisory lock held.**
///
/// * Missing or empty file → [`GENESIS_HASH`].
/// * First line is not a [`ChainedLine`] → legacy pre-chain file: rotate it
///   aside (see [`rotate_legacy`]) and start from [`GENESIS_HASH`].
/// * Otherwise resume from the hash of the last parseable, newline-terminated
///   line. Anything after it (a torn line from a crash or disk-full
///   mid-write) is truncated so the chain stays verifiable.
fn prepare_chain_tip(path: &Path) -> std::io::Result<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GENESIS_HASH.to_string()),
        Err(e) => return Err(e),
    };
    if content.trim().is_empty() {
        return Ok(GENESIS_HASH.to_string());
    }

    let first_line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    if serde_json::from_str::<ChainedLine>(first_line).is_err() {
        rotate_legacy(path)?;
        return Ok(GENESIS_HASH.to_string());
    }

    // Scan forward, remembering the byte offset just past the last complete
    // (newline-terminated) parseable line so a torn tail can be cut off.
    let mut tip = GENESIS_HASH.to_string();
    let mut keep_bytes = 0usize;
    let mut offset = 0usize;
    for segment in content.split_inclusive('\n') {
        let terminated = segment.ends_with('\n');
        let line = segment.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            // Benign blank line: keep it when it directly extends the kept
            // region (verify_chain skips blank lines).
            if terminated && keep_bytes == offset {
                keep_bytes = offset + segment.len();
            }
        } else if terminated {
            if let Ok(parsed) = serde_json::from_str::<ChainedLine>(line) {
                tip = parsed.hash;
                keep_bytes = offset + segment.len();
            }
        }
        offset += segment.len();
    }

    if keep_bytes < content.len() {
        tracing::warn!(
            path = %path.display(),
            truncated_bytes = content.len() - keep_bytes,
            "AuditExecutor: truncating torn/unparsable tail of audit log before appending"
        );
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.set_len(keep_bytes as u64)?;
    }
    Ok(tip)
}

/// Executes [`Effect::PersistAudit`] by appending hash-chained entries to an
/// append-only JSONL log (see the module docs for the line format and for
/// the concurrency/crash-safety behavior).
pub struct AuditExecutor {
    log_path: PathBuf,
    /// Shared mirror of the kernel's audit trail; `None` = checkpoint-marker
    /// mode (historical behavior of [`AuditExecutor::new`]).
    trail: Option<AuditTrailHandle>,
    /// How many dispatch records have already been flushed to disk.
    persisted_records: usize,
    /// How many tool records have already been flushed to disk.
    persisted_tool_records: usize,
}

impl AuditExecutor {
    /// Creates an executor that writes to `log_path`.
    ///
    /// The file is created if it does not exist and opened in append mode.
    /// Without an [`AuditTrailHandle`] each `PersistAudit` appends one chained
    /// `checkpoint` marker entry; use [`Self::with_trail`] to persist the full
    /// audit records.
    pub fn new(log_path: impl Into<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
            trail: None,
            persisted_records: 0,
            persisted_tool_records: 0,
        }
    }

    /// Creates an executor that flushes the full audit records mirrored in
    /// `trail` (dispatch records first, then tool records) on every
    /// `PersistAudit`, each as its own hash-chained JSONL line.
    pub fn with_trail(log_path: impl Into<PathBuf>, trail: AuditTrailHandle) -> Self {
        Self {
            log_path: log_path.into(),
            trail: Some(trail),
            persisted_records: 0,
            persisted_tool_records: 0,
        }
    }

    /// Collects the entries this flush should append, returning them together
    /// with the post-flush cursor positions.
    fn pending_entries(&self, timestamp: &str) -> (Vec<serde_json::Value>, usize, usize) {
        let Some(trail) = &self.trail else {
            let marker = serde_json::json!({
                "kind": "checkpoint",
                "timestamp": timestamp,
            });
            return (
                vec![marker],
                self.persisted_records,
                self.persisted_tool_records,
            );
        };

        // Fetch only the not-yet-persisted suffixes; the trail is
        // append-only, so the cursors stay valid across flushes.
        let records = trail.records_from(self.persisted_records);
        let tool_records = trail.tool_records_from(self.persisted_tool_records);
        let mut entries = Vec::with_capacity(records.len() + tool_records.len());
        for record in &records {
            entries.push(serde_json::json!({
                "kind": "dispatch",
                "timestamp": timestamp,
                "record": record,
            }));
        }
        for record in &tool_records {
            entries.push(serde_json::json!({
                "kind": "tool",
                "timestamp": timestamp,
                "record": record,
            }));
        }
        (
            entries,
            self.persisted_records + records.len(),
            self.persisted_tool_records + tool_records.len(),
        )
    }
}

#[async_trait]
impl EffectExecutor for AuditExecutor {
    async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
        if !matches!(effect, Effect::PersistAudit) {
            return;
        }

        let timestamp = Utc::now().to_rfc3339();
        let (entries, next_records, next_tool_records) = self.pending_entries(&timestamp);
        if entries.is_empty() {
            tracing::debug!(
                path = %self.log_path.display(),
                "AuditExecutor: no new audit records to persist"
            );
            return;
        }

        let path = self.log_path.clone();
        let written = entries.len();

        // Perform the I/O on a blocking thread so we do not block the tokio
        // executor.
        let result = tokio::task::spawn_blocking(move || append_chain(&path, entries)).await;

        match result {
            Ok(Ok(_)) => {
                // Advance the cursors only after a successful write so failed
                // flushes are retried on the next `PersistAudit`.
                self.persisted_records = next_records;
                self.persisted_tool_records = next_tool_records;
                tracing::debug!(
                    path = %self.log_path.display(),
                    entries = written,
                    "AuditExecutor: audit records persisted"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(path = %self.log_path.display(), error = %e, "AuditExecutor: failed to write audit log");
            }
            Err(e) => {
                tracing::warn!(error = %e, "AuditExecutor: spawn_blocking panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sven_hsm::{
        AuditRecord, AuditTrailHandle, Context, Effect, EffectExecutor, Event, EventKind,
        EventSink, Hsm, MachineId, ObservationSink, PermissionPolicy, Principal, Reaction, Runtime,
        ToolAuditRecord, ToolCallId, ToolCapability,
    };

    use super::{verify_chain, AuditExecutor, ChainError, ChainedLine, GENESIS_HASH};

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _: Effect, _: &EventSink, _: &ObservationSink) {}
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct TinyMachine(MachineId);
    impl TinyMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }
    impl sven_hsm::Machine for TinyMachine {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, _ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top | TS::Done => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        Reaction::Handled(vec![])
                    } else {
                        Reaction::Transition {
                            target: TS::Done,
                            effects: vec![],
                            rationale: "done".into(),
                        }
                    }
                }
            }
        }
    }

    /// Spawns a throwaway runtime just to obtain a live `EventSink`.
    fn sink_fixture() -> (Runtime<TinyMachine>, EventSink) {
        let rt = Runtime::spawn(
            Hsm::new(TinyMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        (rt, sink)
    }

    async fn flush(exec: &mut AuditExecutor, sink: &EventSink) {
        exec.execute(Effect::PersistAudit, sink, &ObservationSink::default())
            .await;
    }

    fn parse_lines(path: &std::path::Path) -> Vec<ChainedLine> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn persist_audit_without_trail_writes_chained_checkpoint() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].prev_hash, GENESIS_HASH);
        assert_eq!(lines[0].entry["kind"].as_str(), Some("checkpoint"));
        assert_eq!(verify_chain(&log_path).unwrap(), 1);
        rt.abort();
    }

    #[tokio::test]
    async fn persist_audit_appends_and_links_multiple_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2, "expected exactly 2 lines");
        assert_eq!(
            lines[1].prev_hash, lines[0].hash,
            "second line must chain from the first"
        );
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn non_persist_audit_effect_is_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        exec.execute(
            Effect::EmitInternal {
                name: "test".into(),
                payload: serde_json::Value::Null,
            },
            &sink,
            &ObservationSink::default(),
        )
        .await;

        assert!(
            !log_path.exists(),
            "log should not be created for unhandled effects"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn trail_flushes_full_records_with_principal_and_tool_audit() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let mut ctx = Context::new();
        ctx.principal = Some(Principal::new("acme", "alice"));
        ctx.push_audit(AuditRecord::transition(
            "Idle",
            "Working",
            EventKind::UserMessage,
            &[],
            Some("user asked".into()),
        ));
        ctx.push_tool_audit(ToolAuditRecord::started(
            "Working",
            ToolCallId::new(),
            "read_file",
            ToolCapability::ReadFile,
        ));
        trail.sync_from(&ctx);

        let mut exec = AuditExecutor::with_trail(&log_path, trail);
        let (rt, sink) = sink_fixture();
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2, "one dispatch + one tool entry");

        // The full AuditRecord round-trips, including principal attribution.
        assert_eq!(lines[0].entry["kind"].as_str(), Some("dispatch"));
        let record: AuditRecord = serde_json::from_value(lines[0].entry["record"].clone()).unwrap();
        assert_eq!(record.from_state, "Idle");
        assert_eq!(record.to_state, "Working");
        assert_eq!(record.rationale.as_deref(), Some("user asked"));
        assert_eq!(record.tenant_id.as_deref(), Some("acme"));
        assert_eq!(record.actor_id.as_deref(), Some("alice"));

        // The ToolAuditRecord is attributed too — the durable log must not
        // need call_id correlation to know who ran which tool.
        assert_eq!(lines[1].entry["kind"].as_str(), Some("tool"));
        let tool: ToolAuditRecord =
            serde_json::from_value(lines[1].entry["record"].clone()).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.tenant_id.as_deref(), Some("acme"));
        assert_eq!(tool.actor_id.as_deref(), Some("alice"));

        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn trail_flushes_are_incremental() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let mut ctx = Context::new();
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::UserMessage));
        trail.sync_from(&ctx);

        let mut exec = AuditExecutor::with_trail(&log_path, trail.clone());
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;
        assert_eq!(parse_lines(&log_path).len(), 1);

        // Nothing new: flush writes nothing.
        flush(&mut exec, &sink).await;
        assert_eq!(parse_lines(&log_path).len(), 1, "no duplicate records");

        // One more record: only the delta is appended.
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::Timeout));
        trail.sync_from(&ctx);
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].prev_hash, lines[0].hash);
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn new_executor_resumes_existing_chain() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut first = AuditExecutor::new(&log_path);
        flush(&mut first, &sink).await;

        // A fresh executor instance (e.g. after a restart) must continue the
        // chain from the last line on disk, not restart from genesis.
        let mut second = AuditExecutor::new(&log_path);
        flush(&mut second, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].prev_hash, lines[0].hash);
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn interleaved_executors_never_fork_the_chain() {
        // Regression: two executors appending to the same file used to trust
        // their in-memory prev_hash, so A/B/A interleaving broke the chain at
        // line 3 forever. Every flush must chain from the on-disk tip.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut a = AuditExecutor::new(&log_path);
        let mut b = AuditExecutor::new(&log_path);

        flush(&mut a, &sink).await;
        flush(&mut b, &sink).await;
        flush(&mut a, &sink).await;

        assert_eq!(
            verify_chain(&log_path).unwrap(),
            3,
            "interleaved writers must extend one linear chain"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn legacy_pre_chain_file_is_rotated_so_new_records_verify() {
        // A pre-chain first line used to make verify_chain fail at line 1
        // forever, silently disabling verification for all NEW records too.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        std::fs::write(
            &log_path,
            "{\"kind\":\"PersistAudit\",\"timestamp\":\"2025-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
        assert!(
            verify_chain(&log_path).is_err(),
            "legacy file cannot verify"
        );

        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();
        flush(&mut exec, &sink).await;

        assert_eq!(
            verify_chain(&log_path).unwrap(),
            1,
            "new records must verify after legacy rotation"
        );
        let legacy_rotated = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("audit.jsonl.legacy-")
            });
        assert!(legacy_rotated, "legacy file must be preserved, not deleted");
        rt.abort();
    }

    #[tokio::test]
    async fn torn_trailing_line_is_repaired_on_next_flush() {
        // A crash mid-write leaves a partial (non-newline-terminated) last
        // line. The next flush must truncate it and keep the chain verifiable
        // instead of forking from GENESIS.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut exec = AuditExecutor::new(&log_path);
        flush(&mut exec, &sink).await;
        assert_eq!(verify_chain(&log_path).unwrap(), 1);

        // Simulate a torn write.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"{\"prev_hash\":\"deadbeef\",\"ha").unwrap();
        }
        assert!(verify_chain(&log_path).is_err(), "torn tail breaks verify");

        flush(&mut exec, &sink).await;
        assert_eq!(
            verify_chain(&log_path).unwrap(),
            2,
            "flush must truncate the torn tail and continue the chain"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn read_chain_tolerates_a_torn_unterminated_tail() {
        // Live readers must not report a crash-torn tail as tampering: the
        // torn line is ignored, matching what the next append truncates.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        assert!(
            super::read_chain(&log_path).unwrap().is_empty(),
            "missing file reads as an empty chain"
        );

        let mut exec = AuditExecutor::new(&log_path);
        flush(&mut exec, &sink).await;
        assert_eq!(super::read_chain(&log_path).unwrap().len(), 1);

        // Simulate a torn write (no trailing newline).
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"{\"prev_hash\":\"deadbeef\",\"ha").unwrap();
        }
        assert!(verify_chain(&log_path).is_err(), "strict verify still fails");
        assert_eq!(
            super::read_chain(&log_path).unwrap().len(),
            1,
            "read_chain ignores the torn tail"
        );

        // Real tampering (a newline-terminated bogus line) is still caught.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"ck\":\"bogus\",\"entry\":{}}\n").unwrap();
        }
        assert!(
            super::read_chain(&log_path).is_err(),
            "a terminated bogus line is tampering, not a torn tail"
        );
        rt.abort();
    }

    /// End-to-end: a real erased-runtime session with the default composite
    /// executor must produce a non-empty, verifiable audit log without any
    /// machine emitting `PersistAudit` — the runtime flushes it.
    #[tokio::test]
    async fn erased_runtime_session_writes_verifiable_audit_log() {
        use crate::CompositeExecutorBuilder;
        use sven_hsm::ErasedRuntime;

        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let executor = CompositeExecutorBuilder::default()
            .with_audit_trail(&log_path, trail.clone())
            .build();

        let mut ctx = Context::new();
        ctx.principal = Some(Principal::new("acme", "alice"));
        let rt = ErasedRuntime::spawn_with_audit_trail(
            Box::new(Hsm::new(TinyMachine::new())),
            ctx,
            PermissionPolicy::builder().build(),
            executor,
            16,
            None,
            trail,
        );

        rt.post(Event::UserMessage {
            text: "hello".into(),
        })
        .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(report.state_label, "Done");

        let verified = verify_chain(&log_path).unwrap();
        assert!(
            verified > 0,
            "a real session must leave a non-empty audit log"
        );
        let lines = parse_lines(&log_path);
        let dispatch_line = lines
            .iter()
            .find(|l| l.entry["kind"].as_str() == Some("dispatch"))
            .expect("at least one dispatch record persisted");
        assert_eq!(
            dispatch_line.entry["record"]["tenant_id"].as_str(),
            Some("acme"),
            "persisted records carry principal attribution"
        );
    }

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
            super::canonical_json(&value),
            r#"{"outer_a":"x","outer_z":{"alpha":[{"a":1,"b":2}],"zeta":1}}"#
        );
    }

    #[tokio::test]
    async fn verify_chain_detects_tampering() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let mut ctx = Context::new();
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::UserMessage));
        ctx.push_audit(AuditRecord::ignored("Working", EventKind::Timeout));
        ctx.push_audit(AuditRecord::ignored("Done", EventKind::UserMessage));
        trail.sync_from(&ctx);

        let mut exec = AuditExecutor::with_trail(&log_path, trail);
        let (rt, sink) = sink_fixture();
        flush(&mut exec, &sink).await;
        rt.abort();

        assert_eq!(verify_chain(&log_path).unwrap(), 3);
        let original = std::fs::read_to_string(&log_path).unwrap();

        // Tamper 1: alter the content of the middle record.
        let tampered = original.replacen("\"Working\"", "\"Cover-up\"", 1);
        assert_ne!(tampered, original, "tamper must change the file");
        std::fs::write(&log_path, &tampered).unwrap();
        assert!(
            matches!(
                verify_chain(&log_path),
                Err(ChainError::HashMismatch { line: 2 })
            ),
            "altered record content must be detected on the altered line"
        );

        // Tamper 2: delete the middle line (breaks the prev_hash link).
        let mut lines: Vec<&str> = original.lines().collect();
        lines.remove(1);
        std::fs::write(&log_path, lines.join("\n") + "\n").unwrap();
        assert!(
            matches!(
                verify_chain(&log_path),
                Err(ChainError::BrokenLink { line: 2, .. })
            ),
            "removed line must break the chain"
        );

        // Tamper 3: reorder lines.
        let mut lines: Vec<&str> = original.lines().collect();
        lines.swap(1, 2);
        std::fs::write(&log_path, lines.join("\n") + "\n").unwrap();
        assert!(
            matches!(verify_chain(&log_path), Err(ChainError::BrokenLink { .. })),
            "reordered lines must break the chain"
        );

        // Untampered file still verifies.
        std::fs::write(&log_path, &original).unwrap();
        assert_eq!(verify_chain(&log_path).unwrap(), 3);
    }

    #[test]
    fn verify_chain_of_missing_file_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(verify_chain(dir.path().join("nope.jsonl")).unwrap(), 0);
    }
}
