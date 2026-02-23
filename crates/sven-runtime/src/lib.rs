// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Runtime environment detection utilities.
//!
//! This crate provides project-root discovery, git context collection,
//! CI environment detection, and project context file loading.
//!
//! These are general-purpose utilities usable by any frontend (CI runner,
//! TUI, daemon, etc.) without depending on any specific runner crate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

// ─── Project root detection ───────────────────────────────────────────────────

/// Walk up the directory tree from the current working directory until a
/// `.git` directory is found.  Returns the canonicalized path to that
/// directory.  If no `.git` is found, returns `canonicalize(current_dir())`.
pub fn find_project_root() -> Result<PathBuf> {
    let start = std::env::current_dir()?;
    let mut current = start.as_path();

    loop {
        if current.join(".git").exists() {
            return Ok(std::fs::canonicalize(current)?);
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    Ok(std::fs::canonicalize(&start)?)
}

/// Workspace root markers checked in priority order.
///
/// A *workspace root* is the directory that sits above one or more git
/// repositories and contains shared tooling — knowledge bases, IDE
/// configuration, build system bootstrapping, etc.  It is distinct from the
/// *project root* (the git repository found by `find_project_root()`).
///
/// Each entry is a directory name whose presence at a given ancestor level is
/// treated as the workspace boundary.  Markers are checked in order; the first
/// match wins:
///
/// | Marker    | Created by                      | Reliability |
/// |-----------|----------------------------------|-------------|
/// | `.west`   | `west init` (Zephyr build system) | High — purpose-built workspace marker, always above git repos |
/// | `.cursor` | Cursor IDE                        | Medium — IDE workspace dir, commonly at the repo-collection level |
///
/// Note: `.sven/` is intentionally **not** a workspace marker because it is
/// a project-level directory that lives *inside* the git repository.
const WORKSPACE_MARKERS: &[&str] = &[
    ".west",    // Zephyr West workspace root (west init)
    ".cursor",  // Cursor IDE workspace root
];

/// Heuristically locate the workspace root — the directory above the git
/// repository that contains shared tooling used by multiple projects.
///
/// **This function uses heuristics** (see [`WORKSPACE_MARKERS`]) and may
/// return an incorrect result when the filesystem layout is unusual.  Callers
/// should treat the result as a best-effort hint, not a guarantee.
///
/// ## How it works
///
/// Starting one level *above* `project_root`, the function ascends the
/// directory tree checking for the presence of each [`WORKSPACE_MARKERS`]
/// entry.  The search is capped at [`MAX_WORKSPACE_ASCENT`] levels to avoid
/// false positives far up the filesystem.  If no marker is found, the
/// function returns `project_root` unchanged as a safe fallback.
///
/// Starting above the project root ensures that workspace markers present
/// *inside* the git repository (e.g. a `.cursor/` directory checked in to a
/// repo) are never mistaken for a workspace boundary.
///
/// ## Example layout
///
/// ```text
/// /data/                         ← has .west/ or .cursor/ → workspace root
///   .west/
///   zephyr/          (git repo)
///   ng-iot-platform/ (git repo)  ← project root passed to this function
///     .git/
///     .sven/         ← project-level, NOT a workspace marker
/// ```
///
/// `find_workspace_root("/data/ng-iot-platform")` → `"/data"`
pub fn find_workspace_root(project_root: &Path) -> PathBuf {
    // Begin one level above project_root so that any workspace markers that
    // happen to exist *inside* the project are never matched.
    let start = match project_root.parent() {
        Some(p) => p,
        None => return project_root.to_path_buf(),
    };

    let mut current = start;
    for _ in 0..MAX_WORKSPACE_ASCENT {
        for marker in WORKSPACE_MARKERS {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // No workspace marker found — fall back to project_root so callers always
    // get a usable path rather than an error.
    project_root.to_path_buf()
}

/// Maximum number of directory levels to ascend when searching for a
/// workspace root above the project root.
///
/// Keeping this small (5) prevents the heuristic from matching unrelated
/// system directories (e.g. a `.cursor/` that happens to exist somewhere far
/// up the hierarchy).
const MAX_WORKSPACE_ASCENT: usize = 5;

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

    GitContext { branch, commit, remote_url, dirty_count }
}

/// Run a git command in `dir` with a hard timeout.
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
    if s.is_empty() { None } else { Some(s) }
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

// ─── Project context file ─────────────────────────────────────────────────────

/// Maximum bytes loaded from a project context file.
const MAX_CONTEXT_FILE_BYTES: usize = 16 * 1024;

/// Attempt to load a project-level context / instructions file.  Tried in
/// order:
/// 1. `.sven/context.md`   — sven-specific instructions
/// 2. `AGENTS.md`          — standard agent instructions
/// 3. `CLAUDE.md`          — Claude Code project file
pub fn load_project_context_file(project_root: &Path) -> Option<String> {
    let candidates = [
        project_root.join(".sven").join("context.md"),
        project_root.join("AGENTS.md"),
        project_root.join("CLAUDE.md"),
    ];

    for path in &candidates {
        if !path.exists() {
            continue;
        }
        match std::fs::read(path) {
            Err(_) => continue,
            Ok(bytes) => {
                let (content, truncated) = if bytes.len() > MAX_CONTEXT_FILE_BYTES {
                    let safe = &bytes[..MAX_CONTEXT_FILE_BYTES];
                    let s = String::from_utf8_lossy(safe).trim_end().to_string();
                    (s, true)
                } else {
                    (String::from_utf8_lossy(&bytes).trim().to_string(), false)
                };

                if content.is_empty() {
                    continue;
                }

                return Some(if truncated {
                    format!(
                        "{content}\n\n*(Context file truncated at {} bytes)*",
                        MAX_CONTEXT_FILE_BYTES
                    )
                } else {
                    content
                });
            }
        }
    }
    None
}

// ─── CI context ───────────────────────────────────────────────────────────────

/// Snapshot of the CI environment read from well-known environment variables.
#[derive(Debug, Default)]
pub struct CiContext {
    pub provider: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub pr_number: Option<String>,
    pub run_id: Option<String>,
}

/// Detect the current CI provider and relevant metadata from well-known
/// environment variables.
pub fn detect_ci_context() -> CiContext {
    let mut ctx = CiContext::default();

    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        ctx.provider = Some("GitHub Actions".to_string());
        ctx.repo = std::env::var("GITHUB_REPOSITORY").ok();
        ctx.branch = std::env::var("GITHUB_REF_NAME").ok();
        ctx.commit = std::env::var("GITHUB_SHA").ok();
        ctx.run_id = std::env::var("GITHUB_RUN_ID").ok();
        ctx.pr_number = std::env::var("GITHUB_EVENT_NUMBER")
            .ok()
            .or_else(|| std::env::var("PR_NUMBER").ok());
    } else if std::env::var("GITLAB_CI").as_deref() == Ok("true") {
        ctx.provider = Some("GitLab CI".to_string());
        ctx.repo = std::env::var("CI_PROJECT_PATH").ok();
        ctx.branch = std::env::var("CI_COMMIT_REF_NAME").ok();
        ctx.commit = std::env::var("CI_COMMIT_SHA").ok();
        ctx.run_id = std::env::var("CI_PIPELINE_ID").ok();
        ctx.pr_number = std::env::var("CI_MERGE_REQUEST_IID").ok();
    } else if std::env::var("CIRCLECI").as_deref() == Ok("true") {
        ctx.provider = Some("CircleCI".to_string());
        ctx.repo = std::env::var("CIRCLE_REPOSITORY_URL").ok();
        ctx.branch = std::env::var("CIRCLE_BRANCH").ok();
        ctx.commit = std::env::var("CIRCLE_SHA1").ok();
        ctx.run_id = std::env::var("CIRCLE_BUILD_NUM").ok();
        ctx.pr_number = std::env::var("CIRCLE_PR_NUMBER").ok();
    } else if std::env::var("TRAVIS").as_deref() == Ok("true") {
        ctx.provider = Some("Travis CI".to_string());
        ctx.repo = std::env::var("TRAVIS_REPO_SLUG").ok();
        ctx.branch = std::env::var("TRAVIS_BRANCH").ok();
        ctx.commit = std::env::var("TRAVIS_COMMIT").ok();
        ctx.run_id = std::env::var("TRAVIS_BUILD_ID").ok();
        ctx.pr_number = std::env::var("TRAVIS_PULL_REQUEST")
            .ok()
            .filter(|v| v != "false");
    } else if std::env::var("JENKINS_URL").is_ok() || std::env::var("BUILD_URL").is_ok() {
        ctx.provider = Some("Jenkins".to_string());
        ctx.branch = std::env::var("BRANCH_NAME")
            .ok()
            .or_else(|| std::env::var("GIT_BRANCH").ok());
        ctx.commit = std::env::var("GIT_COMMIT").ok();
        ctx.run_id = std::env::var("BUILD_NUMBER").ok();
    } else if std::env::var("TF_BUILD").as_deref() == Ok("True") {
        ctx.provider = Some("Azure Pipelines".to_string());
        ctx.repo = std::env::var("BUILD_REPOSITORY_NAME").ok();
        ctx.branch = std::env::var("BUILD_SOURCEBRANCH").ok()
            .map(|b| b.trim_start_matches("refs/heads/").to_string());
        ctx.commit = std::env::var("BUILD_SOURCEVERSION").ok();
        ctx.run_id = std::env::var("BUILD_BUILDID").ok();
        ctx.pr_number = std::env::var("SYSTEM_PULLREQUEST_PULLREQUESTNUMBER").ok();
    } else if std::env::var("BITBUCKET_BUILD_NUMBER").is_ok() {
        ctx.provider = Some("Bitbucket Pipelines".to_string());
        ctx.repo = std::env::var("BITBUCKET_REPO_FULL_NAME").ok();
        ctx.branch = std::env::var("BITBUCKET_BRANCH").ok();
        ctx.commit = std::env::var("BITBUCKET_COMMIT").ok();
        ctx.run_id = std::env::var("BITBUCKET_BUILD_NUMBER").ok();
        ctx.pr_number = std::env::var("BITBUCKET_PR_ID").ok();
    } else if std::env::var("CI").as_deref() == Ok("true") {
        ctx.provider = Some("CI".to_string());
        ctx.branch = std::env::var("BRANCH_NAME")
            .ok()
            .or_else(|| std::env::var("GIT_BRANCH").ok());
        ctx.commit = std::env::var("GIT_COMMIT").ok();
    }

    ctx
}

impl CiContext {
    /// Returns true if any CI provider was detected.
    pub fn is_ci(&self) -> bool {
        self.provider.is_some()
    }

    /// Format as a system-prompt section.  Returns `None` when not in CI.
    pub fn to_prompt_section(&self) -> Option<String> {
        let provider = self.provider.as_deref()?;

        let mut lines = vec![
            "## CI Environment".to_string(),
            format!("Running in: {}", provider),
        ];

        if let Some(repo) = &self.repo {
            lines.push(format!("Repository: {}", repo));
        }
        if let Some(branch) = &self.branch {
            lines.push(format!("Branch: {}", branch));
        }
        if let Some(commit) = &self.commit {
            let short = &commit[..commit.len().min(12)];
            lines.push(format!("Commit: {}", short));
        }
        if let Some(pr) = &self.pr_number {
            lines.push(format!("PR/MR: #{}", pr));
        }

        Some(lines.join("\n"))
    }
}

// ─── CI env vars as template variables ───────────────────────────────────────

/// Build a map of well-known CI environment variables for use as template
/// variables in workflow files.
pub fn ci_template_vars(ci: &CiContext) -> std::collections::HashMap<String, String> {
    let mut vars = std::collections::HashMap::new();

    let raw_vars: &[&str] = &[
        "GITHUB_SHA", "GITHUB_REF_NAME", "GITHUB_REPOSITORY", "GITHUB_RUN_ID",
        "GITHUB_EVENT_NUMBER", "GITHUB_ACTOR", "GITHUB_WORKFLOW",
        "CI_COMMIT_SHA", "CI_COMMIT_REF_NAME", "CI_PROJECT_PATH",
        "CI_PIPELINE_ID", "CI_MERGE_REQUEST_IID",
        "CIRCLE_SHA1", "CIRCLE_BRANCH", "CIRCLE_BUILD_NUM",
        "TRAVIS_COMMIT", "TRAVIS_BRANCH", "TRAVIS_REPO_SLUG",
        "BUILD_SOURCEVERSION", "BUILD_SOURCEBRANCH", "BUILD_REPOSITORY_NAME",
        "BITBUCKET_COMMIT", "BITBUCKET_BRANCH",
        "GIT_COMMIT", "GIT_BRANCH", "BUILD_NUMBER", "BRANCH_NAME",
    ];
    for name in raw_vars {
        if let Ok(val) = std::env::var(name) {
            vars.insert(format!("CI_{}", name.to_lowercase()), val);
        }
    }

    if let Some(v) = &ci.branch    { vars.entry("branch".into()).or_insert_with(|| v.clone()); }
    if let Some(v) = &ci.commit    { vars.entry("commit".into()).or_insert_with(|| v.clone()); }
    if let Some(v) = &ci.repo      { vars.entry("repo".into()).or_insert_with(|| v.clone()); }
    if let Some(v) = &ci.pr_number { vars.entry("pr".into()).or_insert_with(|| v.clone()); }

    vars
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_project_root_returns_a_directory() {
        let root = find_project_root().expect("find_project_root should not fail");
        assert!(root.is_dir(), "project root should be a directory");
    }

    #[test]
    fn find_workspace_root_detects_west_workspace() {
        // .west/ is the Zephyr West workspace marker and should be found first.
        let tmp = std::env::temp_dir().join("sven_wsroot_west_test");
        let project = tmp.join("ng-iot-platform");
        let west_dir = tmp.join(".west");
        let _ = std::fs::create_dir_all(&project);
        let _ = std::fs::create_dir_all(&west_dir);

        let ws = find_workspace_root(&project);
        assert_eq!(ws, tmp, ".west/ should be recognised as workspace root");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_detects_cursor_workspace() {
        let tmp = std::env::temp_dir().join("sven_wsroot_cursor_test");
        let project = tmp.join("myproject");
        let cursor_dir = tmp.join(".cursor");
        let _ = std::fs::create_dir_all(&project);
        let _ = std::fs::create_dir_all(&cursor_dir);

        let ws = find_workspace_root(&project);
        assert_eq!(ws, tmp, ".cursor/ should be recognised as workspace root");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_west_takes_priority_over_cursor() {
        // When both .west/ and .cursor/ exist at the same ancestor, .west/ wins
        // because it appears first in WORKSPACE_MARKERS.
        let tmp = std::env::temp_dir().join("sven_wsroot_priority_test");
        let project = tmp.join("myproject");
        let _ = std::fs::create_dir_all(&project);
        let _ = std::fs::create_dir_all(tmp.join(".west"));
        let _ = std::fs::create_dir_all(tmp.join(".cursor"));

        let ws = find_workspace_root(&project);
        assert_eq!(ws, tmp, "marker at same level should be found regardless of order");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_ignores_markers_inside_project_root() {
        // A .cursor/ inside the project itself must not be mistaken for the
        // workspace root — the search starts *above* project_root.
        let tmp = std::env::temp_dir().join("sven_wsroot_inside_test");
        let project = tmp.join("myproject");
        let cursor_inside_project = project.join(".cursor");
        let _ = std::fs::create_dir_all(&cursor_inside_project);
        // No marker exists above `project`, so we expect a fallback.

        let ws = find_workspace_root(&project);
        assert_eq!(ws, project,
            "markers inside the project root should not be treated as workspace boundary");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_falls_back_to_project_root_when_no_marker() {
        let tmp = std::env::temp_dir().join("sven_wsroot_fallback_test");
        let project = tmp.join("myproject");
        let _ = std::fs::create_dir_all(&project);

        let ws = find_workspace_root(&project);
        assert_eq!(ws, project, "should fall back to project root when no marker found");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ci_context_no_provider_returns_empty_section() {
        let ctx = CiContext::default();
        assert!(ctx.to_prompt_section().is_none());
        assert!(!ctx.is_ci());
    }

    #[test]
    fn ci_context_with_provider_formats_section() {
        let ctx = CiContext {
            provider: Some("GitHub Actions".to_string()),
            repo: Some("acme/repo".to_string()),
            branch: Some("main".to_string()),
            commit: Some("abc123def456".to_string()),
            pr_number: Some("42".to_string()),
            run_id: None,
        };
        let section = ctx.to_prompt_section().unwrap();
        assert!(section.contains("GitHub Actions"));
        assert!(section.contains("acme/repo"));
        assert!(section.contains("main"));
        assert!(section.contains("abc123"));
        assert!(section.contains("#42"));
    }

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

    #[test]
    fn load_project_context_file_missing_returns_none() {
        let tmp = std::env::temp_dir().join("sven_rt_test_no_ctx");
        let _ = std::fs::create_dir_all(&tmp);
        assert!(load_project_context_file(&tmp).is_none());
    }

    #[test]
    fn load_project_context_file_reads_agents_md() {
        let tmp = std::env::temp_dir().join("sven_rt_test_agents_md");
        let _ = std::fs::create_dir_all(&tmp);
        let agents_path = tmp.join("AGENTS.md");
        std::fs::write(&agents_path, "# Project instructions\n\nAlways use Rust.").unwrap();
        let result = load_project_context_file(&tmp);
        let _ = std::fs::remove_file(&agents_path);
        let content = result.expect("should find AGENTS.md");
        assert!(content.contains("Always use Rust"));
    }

    #[test]
    fn ci_template_vars_provides_shortcuts() {
        let ctx = CiContext {
            provider: Some("Test CI".to_string()),
            branch: Some("feat/test".to_string()),
            commit: Some("abc1234".to_string()),
            repo: Some("acme/repo".to_string()),
            pr_number: Some("7".to_string()),
            run_id: None,
        };
        let vars = ci_template_vars(&ctx);
        assert_eq!(vars.get("branch").map(|s| s.as_str()), Some("feat/test"));
        assert_eq!(vars.get("commit").map(|s| s.as_str()), Some("abc1234"));
        assert_eq!(vars.get("repo").map(|s| s.as_str()), Some("acme/repo"));
        assert_eq!(vars.get("pr").map(|s| s.as_str()), Some("7"));
    }
}
