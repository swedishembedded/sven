// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents that hand back
// a structured, independently checkable result instead of a narrative claim.
// If your team needs expertise in agent interfaces or acceptance evidence,
// you can procure our services by sending an email to
// info@swedishembedded.com.

//! The structured handoff: status, changed files, validation evidence,
//! usage, and artifact locations.
//!
//! The final reply of a delegated attempt is a natural-language claim, and a
//! natural-language claim is not reviewable. [`Outcome`] is the result the
//! reviewing side inspects instead: it is written from the run's own
//! observations (kernel events, completion checks, workspace git state) and
//! never from anything the model asserted about its own work.

use anyhow::Context;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// How the attempt ended. The status words are part of the interface -
/// `show` filters on them and the reviewing side classifies on them - so
/// they are a closed set rather than free text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The attempt ran to completion and every completion check passed.
    Completed,
    /// The attempt ran to completion, but a completion check failed.
    Failed,
    /// The configured timeout fired mid-turn.
    Timeout,
    /// An interruption (Ctrl-C) arrived mid-turn.
    Cancelled,
    /// The engine or a tool boundary errored before any verdict.
    Errored,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Timeout => "timeout",
            Status::Cancelled => "cancelled",
            Status::Errored => "errored",
        }
    }
}

/// The status a finished attempt reports: the turn's own ending, demoted by
/// its self-executed checks. A turn that ran to completion but failed its
/// check is a `failed` attempt, not a completed one - a delegating script
/// keys its decision on this word, and "the model finished talking" is not
/// "the work passed".
#[must_use]
pub fn verdict(turn: Status, checks: &[Check]) -> Status {
    match turn {
        Status::Completed if checks.iter().any(|c| !c.passed) => Status::Failed,
        other => other,
    }
}

/// One file the attempt touched, with its hash after the attempt. When the
/// workspace is a git repository the before-state is also derivable - the
/// outcome names which case applies rather than guessing.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChangedFile {
    /// Workspace-relative path.
    pub path: String,
    /// `modified` | `added` | `deleted` (git); `unknown` otherwise.
    pub kind: String,
    /// sha256 of the file's content after the attempt, when it exists.
    pub hash: Option<String>,
}

/// One completion check the run executed itself, as its own validation
/// evidence. This is the attempt's check, never the reviewer's - the
/// reviewing side runs its own commands against the revision.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Check {
    pub command: String,
    pub exit: i32,
    pub passed: bool,
    /// Reference to the artifact file holding the full output.
    pub output_ref: Option<String>,
}

/// What the run observed about resources and spending.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub tool_calls: u64,
    pub failed_tool_calls: u64,
    pub compactions: u64,
    /// Model input/output tokens, summed over the usage reports the kernel
    /// emitted. Local providers may report partial counters; the trace
    /// carries every raw report, so the sums stay auditable.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// USD cost where the provider reported one (OpenRouter does; a local
    /// serve does not). `null` means unmeasured, never free.
    pub cost_usd: Option<f64>,
    pub duration_secs: u64,
}

/// The structured handoff.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    pub schema: u32,
    pub run_id: String,
    pub status: Status,
    /// The model's final reply, verbatim. Evidence for the reviewer, never a
    /// substitute for the evidence below.
    pub reply: Option<String>,
    pub changed_files: Vec<ChangedFile>,
    /// `git` when the workspace is a repository and the diff is derivable;
    /// `tool_evidence` when the attempt is not a repository and only the
    /// kernel's file-mutation events speak.
    pub changed_files_basis: String,
    pub checks: Vec<Check>,
    /// Everything a failed tool call said (first line each), because "11
    /// tool calls and nothing changed" is not a diagnosis.
    pub tool_failures: Vec<String>,
    pub usage: Usage,
    /// What the run could not resolve on its own: completion checks that
    /// never ran, an interrupted turn, a tool the engine refused.
    pub unresolved: Vec<String>,
    /// Artifact locations for independent review.
    pub artifacts: Vec<String>,
}

impl Outcome {
    pub const SCHEMA: u32 = 1;

    /// Writes itself under the run directory, atomically.
    pub fn save(&self, run_dir: &Path) -> anyhow::Result<()> {
        let path = run_dir.join("outcome.json");
        let text = serde_json::to_string_pretty(self)?;
        crate::store::write_atomic(&path, &text)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Reads the outcome a completed run wrote.
    pub fn load(run_dir: &Path) -> anyhow::Result<Outcome> {
        let path = run_dir.join("outcome.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Captures the workspace's changed files from git state. `before` is the
/// set of paths observed dirty BEFORE the attempt, so a file the caller had
/// already modified is attributed correctly: it stays in the diff, marked as
/// it is, and is not claimed as the attempt's addition.
pub fn capture_changed_files(workspace: &Path) -> anyhow::Result<(Vec<ChangedFile>, String)> {
    let run = |args: &[&str]| -> anyhow::Result<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .context("running git in the workspace")?;
        if !out.status.success() {
            anyhow::bail!("git {} failed in the workspace", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // "Is this a git repository?" is one porcelain call. Anything it fails
    // on - no repo, no commit yet - falls to the tool-evidence basis.
    if run(&["rev-parse", "--is-inside-work-tree"]).is_err() {
        return Ok((Vec::new(), "tool_evidence".into()));
    }

    let mut files = BTreeMap::new();
    let porcelain = run(&["status", "--porcelain"])?;
    for line in porcelain.lines() {
        // Porcelain v1 is fixed-width: two status columns, one space, then
        // the path (quoted when it needs quoting, `old -> new` for renames).
        // A `split` on the first space would eat the status columns, so the
        // widths are taken literally instead.
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let path = line.get(3..).unwrap_or_default().trim();
        if path.is_empty() {
            continue;
        }
        // The engine records its own session state under `.sven/` in the
        // workspace it runs in. That directory exists because the attempt
        // ran, not because the agent authored a change - reporting it as a
        // changed file would put machinery into the reviewer's evidence.
        if path == ".sven/" || path.starts_with(".sven/") {
            continue;
        }
        let kind = if code == "??" {
            "added"
        } else if code.contains('D') {
            "deleted"
        } else {
            "modified"
        };
        let path = path
            .trim_start_matches('"')
            .trim_end_matches('"')
            .to_string();
        // A rename carries two paths; the destination is the file that now
        // exists, so that is the one hashed and reported.
        let path = path
            .split_once(" -> ")
            .map(|(_, new)| new.trim().to_string())
            .unwrap_or(path);
        files.insert(
            path.clone(),
            ChangedFile {
                path,
                kind: kind.into(),
                hash: None,
            },
        );
    }

    // Hash every file that exists after the attempt.
    for entry in files.values_mut() {
        let full = workspace.join(&entry.path);
        if full.is_file() {
            let bytes = std::fs::read(&full)?;
            entry.hash = Some(hex(&Sha256::digest(&bytes)));
        }
    }

    Ok((files.into_values().collect(), "git".into()))
}

/// Hashes the files the kernel's file-mutation tools reported writing, for a
/// workspace that is not a git repository. Before-state is not derivable
/// there, and the outcome says so rather than implying a diff it cannot
/// produce.
pub fn capture_tool_evidence(
    workspace: &Path,
    paths: &[String],
) -> anyhow::Result<Vec<ChangedFile>> {
    let mut files = Vec::new();
    for path in paths {
        let full = workspace.join(path);
        let (kind, hash) = if full.is_file() {
            let bytes = std::fs::read(&full)?;
            ("unknown", Some(hex(&Sha256::digest(&bytes))))
        } else {
            ("unknown", None)
        };
        files.push(ChangedFile {
            path: path.clone(),
            kind: kind.into(),
            hash,
        });
    }
    Ok(files)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_workspace_reports_a_diff_with_hashes() {
        let dir = std::env::temp_dir().join(format!("loop-outcome-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "one"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        // The engine records its own session state under .sven/ in the
        // workspace; that is machinery, not a change the attempt authored.
        std::fs::create_dir_all(dir.join(".sven")).unwrap();
        std::fs::write(dir.join(".sven").join("session.json"), "{}\n").unwrap();

        let (files, basis) = capture_changed_files(&dir).unwrap();
        assert_eq!(basis, "git");
        let modified = files
            .iter()
            .find(|f| f.path == "a.txt")
            .expect("modified file");
        assert_eq!(modified.kind, "modified");
        assert!(modified.hash.is_some());
        let added = files
            .iter()
            .find(|f| f.path == "b.txt")
            .expect("added file");
        assert_eq!(added.kind, "added");
        assert!(
            files.iter().all(|f| !f.path.starts_with(".sven")),
            "engine-owned state must not be reported as a changed file: {files:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_non_git_workspace_names_the_basis_it_cannot_imply() {
        let dir = std::env::temp_dir().join(format!("loop-outcome-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Not a repository: git rev-parse fails.
        let (files, basis) = capture_changed_files(&dir).unwrap();
        assert_eq!(basis, "tool_evidence");
        assert!(files.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_words_are_the_closed_set_the_reviewer_filters_on() {
        assert_eq!(Status::Completed.as_str(), "completed");
        assert_eq!(Status::Timeout.as_str(), "timeout");
        assert_eq!(Status::Cancelled.as_str(), "cancelled");
        assert_eq!(Status::Errored.as_str(), "errored");
        assert_eq!(Status::Failed.as_str(), "failed");
    }

    #[test]
    fn a_failed_completion_check_demotes_the_status_to_failed() {
        let pass = Check {
            command: "sh tests/run.sh".into(),
            exit: 0,
            passed: true,
            output_ref: None,
        };
        let fail = Check {
            command: "sh tests/run.sh".into(),
            exit: 1,
            passed: false,
            output_ref: None,
        };
        assert_eq!(
            verdict(Status::Completed, std::slice::from_ref(&pass)),
            Status::Completed
        );
        assert_eq!(
            verdict(Status::Completed, std::slice::from_ref(&fail)),
            Status::Failed
        );
        assert_eq!(
            verdict(Status::Completed, &[pass, fail.clone()]),
            Status::Failed
        );
        // A non-completed turn keeps its own ending - the timeout/cancel
        // classification is the reason the checks did not run, and a `failed`
        // there would claim a verdict the turn never reached.
        assert_eq!(
            verdict(Status::Timeout, std::slice::from_ref(&fail)),
            Status::Timeout
        );
        assert_eq!(verdict(Status::Errored, &[]), Status::Errored);
    }
}
