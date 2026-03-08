// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Git worktree isolation for agent teammates.
//!
//! Each spawned teammate gets its own Git worktree so that concurrent edits
//! do not interfere.  The branch naming convention is:
//!
//!   `sven/team-{team_name}/{role}-{agent_name}`
//!
//! Worktrees are created under `{repo_root}/.sven-worktrees/{team_name}/`.
//!
//! The lead agent merges finished branches with [`merge_teammate_branch`].

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Subdirectory under the repo root where worktrees are placed.
const WORKTREE_DIR: &str = ".sven-worktrees";

// ── Branch naming ─────────────────────────────────────────────────────────────

fn sanitize_branch_component(s: &str) -> String {
    s.chars()
        .map(|c| if c == ' ' || c == '/' { '-' } else { c })
        .collect()
}

/// Return the canonical branch name for a teammate.
///
/// Format: `sven/team-{team_name}/{role}-{agent_name}`
pub fn teammate_branch_name(team_name: &str, role: &str, agent_name: &str) -> String {
    let safe_name = sanitize_branch_component(agent_name);
    let safe_role = sanitize_branch_component(role);
    let safe_team = sanitize_branch_component(team_name);
    format!("sven/team-{safe_team}/{safe_role}-{safe_name}")
}

/// Return the worktree path for a teammate.
///
/// Path: `{repo_root}/.sven-worktrees/{team_name}/{role}-{agent_name}`
pub fn teammate_worktree_path(
    repo_root: &Path,
    team_name: &str,
    role: &str,
    agent_name: &str,
) -> PathBuf {
    let safe_name = sanitize_branch_component(agent_name);
    let safe_role = sanitize_branch_component(role);
    let safe_team = sanitize_branch_component(team_name);
    repo_root
        .join(WORKTREE_DIR)
        .join(safe_team)
        .join(format!("{safe_role}-{safe_name}"))
}

// ── WorktreeGuard ─────────────────────────────────────────────────────────────

/// RAII guard that removes the worktree when dropped.
///
/// Created by [`create_teammate_worktree`].  Pass `into_path()` to the spawned
/// process as its working directory so it operates inside the isolated tree.
pub struct WorktreeGuard {
    path: PathBuf,
    branch: String,
    repo_root: PathBuf,
}

impl WorktreeGuard {
    /// The working directory of this worktree.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The branch created for this worktree.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Release the guard without removing the worktree.
    ///
    /// Call this when the agent exits cleanly and the lead will merge later.
    pub fn into_path(self) -> PathBuf {
        let path = self.path.clone();
        std::mem::forget(self);
        path
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let _ = remove_worktree(&self.repo_root, &self.path, &self.branch);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Create a Git worktree for a teammate.
///
/// Steps:
/// 1. Find the repository root with `git rev-parse --show-toplevel`.
/// 2. Create the branch `sven/team-{team}/{role}-{name}` from `HEAD`.
/// 3. Add the worktree at `.sven-worktrees/{team}/{role}-{name}`.
///
/// Returns a [`WorktreeGuard`] that removes the worktree on drop, plus the
/// repository root (needed for the merge step).
pub fn create_teammate_worktree(
    working_dir: &Path,
    team_name: &str,
    role: &str,
    agent_name: &str,
) -> anyhow::Result<WorktreeGuard> {
    let repo_root = find_repo_root(working_dir)?;
    let branch = teammate_branch_name(team_name, role, agent_name);
    let wt_path = teammate_worktree_path(&repo_root, team_name, role, agent_name);

    // Ensure parent directory exists.
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree parent {}", parent.display()))?;
    }

    // Create a new branch from HEAD and set up the worktree in one command.
    let status = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            wt_path.to_str().unwrap_or_default(),
            "HEAD",
        ])
        .current_dir(&repo_root)
        .status()
        .with_context(|| "running git worktree add")?;

    if !status.success() {
        anyhow::bail!(
            "git worktree add failed for branch {:?} at {}",
            branch,
            wt_path.display()
        );
    }

    Ok(WorktreeGuard {
        path: wt_path,
        branch,
        repo_root,
    })
}

/// Remove a worktree and delete the associated branch.
///
/// Called automatically by [`WorktreeGuard::drop`] and by the cleanup tool.
pub fn remove_worktree(repo_root: &Path, wt_path: &Path, branch: &str) -> anyhow::Result<()> {
    // git worktree remove --force <path>
    let _ = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            wt_path.to_str().unwrap_or_default(),
        ])
        .current_dir(repo_root)
        .status();

    // Delete the branch (ignore errors — branch may not exist yet if the
    // process crashed before the first commit).
    let _ = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(repo_root)
        .status();

    Ok(())
}

/// List all worktrees that belong to a team.
///
/// Returns `(worktree_path, branch_name)` pairs.
pub fn list_team_worktrees(
    repo_root: &Path,
    team_name: &str,
) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let prefix = format!("sven/team-{team_name}/");
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .with_context(|| "running git worktree list")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let mut current_path: Option<PathBuf> = None;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            if rest.starts_with(&prefix) {
                if let Some(path) = current_path.take() {
                    results.push((path, rest.to_string()));
                }
            } else {
                current_path = None;
            }
        }
    }

    Ok(results)
}

// ── Merge coordination ────────────────────────────────────────────────────────

/// Merge a teammate's branch into the current branch (from the lead's checkout).
///
/// Uses `git merge --no-ff` so that merge commits are always created — this
/// preserves the per-teammate commit history for audit purposes.
///
/// Returns `Ok(message)` on success, `Err` with conflict details on failure.
pub fn merge_teammate_branch(
    repo_root: &Path,
    branch: &str,
    commit_message: Option<&str>,
) -> anyhow::Result<String> {
    let mut args = vec![
        "merge".to_string(),
        "--no-ff".to_string(),
        branch.to_string(),
    ];
    if let Some(msg) = commit_message {
        args.push("-m".to_string());
        args.push(msg.to_string());
    }

    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running git merge {branch}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(format!("Merged branch {branch:?}.\n{stdout}"))
    } else {
        Err(anyhow::anyhow!(
            "git merge failed for branch {branch:?}:\n{stdout}\n{stderr}"
        ))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Locate the Git repository root by running `git rev-parse --show-toplevel`.
pub fn find_repo_root(working_dir: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(working_dir)
        .output()
        .with_context(|| "running git rev-parse --show-toplevel")?;

    if !output.status.success() {
        anyhow::bail!(
            "Not inside a Git repository (cwd: {})",
            working_dir.display()
        );
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(path))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_format() {
        let b = teammate_branch_name("auth-refactor", "reviewer", "sec-reviewer");
        assert_eq!(b, "sven/team-auth-refactor/reviewer-sec-reviewer");
    }

    #[test]
    fn branch_name_sanitises_spaces() {
        let b = teammate_branch_name("my team", "test runner", "agent one");
        assert_eq!(b, "sven/team-my-team/test-runner-agent-one");
    }

    #[test]
    fn worktree_path_is_inside_repo() {
        let root = PathBuf::from("/repo");
        let path = teammate_worktree_path(&root, "auth", "reviewer", "sec");
        assert!(path.starts_with(&root));
        assert!(path.to_string_lossy().contains("reviewer-sec"));
    }
}
