// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Git context collection: branch, commit, remote URL, dirty-file count.

use std::path::Path;
use std::time::Duration;

// ─── Git context ──────────────────────────────────────────────────────────────

/// Maximum bytes read from a single git sub-command output.
const GIT_OUTPUT_LIMIT: usize = 4 * 1024;

/// Per-command timeout for git sub-processes.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Live state of the git repository at the project root.
#[derive(Debug, Default)]
pub struct GitContext {
    pub branch: Option<String>,
    /// Short (7-char) commit hash.
    pub commit: Option<String>,
    pub remote_url: Option<String>,
    /// Number of changed / untracked files reported by `git status --porcelain`.
    pub dirty_count: usize,
}

/// Collect git metadata from the repository at `project_root` by running git
/// sub-processes with a per-command timeout.
///
/// Returns a default (empty) `GitContext` if git is not available, the
/// directory is not a repository, or the commands time out.
pub fn collect_git_context(project_root: &Path) -> GitContext {
    let branch = run_git_timed(&["rev-parse", "--abbrev-ref", "HEAD"], project_root);
    let commit = run_git_timed(&["rev-parse", "--short", "HEAD"], project_root);
    let remote_url = run_git_timed(&["remote", "get-url", "origin"], project_root);
    let dirty_count = run_git_timed(&["status", "--porcelain"], project_root)
        .map(|s| s.lines().count())
        .unwrap_or(0);

    GitContext {
        branch,
        commit,
        remote_url,
        dirty_count,
    }
}

/// Run a git command in `dir` with a hard timeout.
///
/// Exposed as `pub(crate)` so that sub-modules (e.g. `knowledge`) can reuse
/// it without duplicating the timeout logic.
pub(crate) fn run_git_timed_pub(args: &[&str], dir: &Path) -> Option<String> {
    run_git_timed(args, dir)
}

fn run_git_timed(args: &[&str], dir: &Path) -> Option<String> {
    use std::sync::mpsc;
    use std::thread;

    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = std::process::Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output();
        let _ = tx.send(result);
    });

    let output = rx.recv_timeout(GIT_TIMEOUT).ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(GIT_OUTPUT_LIMIT)]);
    let s = raw.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl GitContext {
    /// Returns true if no git data was found.
    pub fn is_empty(&self) -> bool {
        self.branch.is_none() && self.commit.is_none()
    }

    /// Format as a system-prompt section.  Returns `None` for an empty context.
    pub fn to_prompt_section(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut lines = vec!["## Git Context".to_string()];
        if let Some(branch) = &self.branch {
            lines.push(format!("Branch: {branch}"));
        }
        if let Some(commit) = &self.commit {
            lines.push(format!("Commit: {commit}"));
        }
        if let Some(remote) = &self.remote_url {
            lines.push(format!("Remote: {remote}"));
        }
        if self.dirty_count > 0 {
            lines.push(format!("Uncommitted changes: {} file(s)", self.dirty_count));
        } else if self.commit.is_some() {
            lines.push("Working tree: clean".to_string());
        }
        Some(lines.join("\n"))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_context_empty_returns_none_section() {
        let ctx = GitContext::default();
        assert!(ctx.is_empty());
        assert!(ctx.to_prompt_section().is_none());
    }

    #[test]
    fn git_context_with_data_formats_section() {
        let ctx = GitContext {
            branch: Some("feat/headless".to_string()),
            commit: Some("d3adb33".to_string()),
            remote_url: Some("git@github.com:acme/sven.git".to_string()),
            dirty_count: 3,
        };
        let section = ctx.to_prompt_section().unwrap();
        assert!(section.contains("Git Context"));
        assert!(section.contains("feat/headless"));
        assert!(section.contains("d3adb33"));
        assert!(section.contains("3 file(s)"));
    }

    #[test]
    fn git_context_clean_working_tree_label() {
        let ctx = GitContext {
            branch: Some("main".to_string()),
            commit: Some("abc1234".to_string()),
            remote_url: None,
            dirty_count: 0,
        };
        let section = ctx.to_prompt_section().unwrap();
        assert!(section.contains("clean"));
    }
}
