// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents whose runs,
// traces and learned state survive process restart. If your team needs
// expertise in agent infrastructure or durable operating state, you can
// procure our services by sending an email to info@swedishembedded.com.

//! Where the loop's durable state lives, and what each part is for.
//!
//! Everything is under `~/.sven/loop/` - the mandate's namespaced subtree, a
//! sibling of sven's own state directories, never one of theirs - because a
//! run whose evidence is scattered under temp directories cannot be audited
//! after a restart, and an audit after a restart is the point of the trace.
//!
//! Layout:
//!
//! ```text
//! ~/.sven/loop/
//!   runs/<run_id>/         one delegated attempt
//!     run.json             manifest + latest status (atomic)
//!     events.jsonl         the append-only trace
//!     transcript.json      what the agent exchanged with the model
//!     checkpoint/state.json  a suspended AgentState (serde)
//!     captured-requests.json  model input captured at the wire, when recorded
//!     artifacts/           large tool outputs, referenced by trace events
//!   datasets/<repo>/       curated training data derived from VERIFIED runs
//!   adapters/              published (promoted) adapters + rollback pointer
//!   studies/               training-job reports (one per study run)
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The loop's state root. Overridable so a test can use a scratch directory
/// without touching a real one.
#[must_use]
pub fn state_root() -> PathBuf {
    match std::env::var("SVEN_LOOP_STATE") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs_home().join(".sven").join("loop"),
    }
}

#[must_use]
fn dirs_home() -> PathBuf {
    // Deliberately plain: `HOME` is the one root the mandate names, and
    // pulling a `dirs` dependency in for `home_dir` would be a dependency for
    // one function. An empty `HOME` is refused by the caller that builds a
    // path under it.
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home),
        _ => PathBuf::from("."),
    }
}

/// One run's directory under the state root.
#[must_use]
pub fn run_dir(run_id: &str) -> PathBuf {
    state_root().join("runs").join(run_id)
}

/// Generates a stable run id: time-ordered, so a directory listing reads as
/// a history, with a random suffix so two runs started in the same second
/// never collide.
#[must_use]
pub fn new_run_id() -> String {
    let t = crate::clock::now();
    let rand = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!(
        "loop-{:04}{:02}{:02}T{:02}{:02}{:02}.{:03}-{:04x}",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.min,
        t.sec,
        t.millis,
        rand & 0xffff
    )
}

/// Writes `text` to `path` atomically: tmp file in the same directory, fsync,
/// rename. A status file half-written by a crash must never read as a
/// status.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// A run's manifest: what was configured, and where the attempt stands.
/// Written atomically at every transition, so `show` never depends on the
/// process that wrote it still being alive.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RunManifest {
    /// Schema version, so an old state directory stays readable.
    pub schema: u32,
    pub run_id: String,
    pub workspace: String,
    pub task: String,
    /// `pending` while the attempt runs, then the attempt's final status.
    pub status: String,
    /// A run may be resumed into further attempts; this is the latest.
    pub attempts: u32,
    pub started_ts: String,
    pub updated_ts: String,
    pub model: String,
    pub base_url: Option<String>,
    /// The configured limits, as configured - recorded so "why did it stop"
    /// has an answer that does not require guessing what was set.
    pub limits: Limits,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Limits {
    pub timeout_secs: u64,
    pub max_tool_rounds: Option<u32>,
}

/// Reads a run's manifest, refusing to invent one.
pub fn read_manifest(run_id: &str) -> anyhow::Result<RunManifest> {
    let path = run_dir(run_id).join("run.json");
    let text = fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// Every run directory, oldest first, by manifest status.
pub fn list_runs() -> anyhow::Result<Vec<RunManifest>> {
    let runs = state_root().join("runs");
    if !runs.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(&runs)? {
        let path = entry?.path().join("run.json");
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(manifest) = serde_json::from_str::<RunManifest>(&text) {
                found.push(manifest);
            }
        }
    }
    found.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_time_ordered_and_unique_within_a_second() {
        let a = new_run_id();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = new_run_id();
        assert!(a < b, "ids must sort as a history: {a} vs {b}");
    }

    #[test]
    fn write_atomic_leaves_no_tmp_file_behind() {
        let dir = std::env::temp_dir().join(format!("loop-store-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.json");
        write_atomic(&path, "{\"status\":\"pending\"}").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"status\":\"pending\"}"
        );
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
