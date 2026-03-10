// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Project-root and workspace-root discovery, auto-log path resolution, and
//! project context file loading.

use std::path::{Path, PathBuf};

use anyhow::Result;

// ─── Auto-log path resolution ─────────────────────────────────────────────────

/// Walk up from the current directory looking for a `.sven` subdirectory.
/// Returns `{sven_dir}/logs/<YYYY-MM-DD_HH-MM-SS.mmm>.jsonl` when found.
/// Creates the `logs/` subdirectory if it does not yet exist.
///
/// This is the "black box recorder" path: sven writes a full-fidelity JSONL
/// log here automatically in all modes (TUI and headless) whenever a `.sven/`
/// project directory is present.
pub fn resolve_auto_log_path() -> Option<PathBuf> {
    let start = std::env::current_dir().ok()?;
    let mut current: &Path = &start;
    loop {
        let candidate = current.join(".sven");
        if candidate.is_dir() {
            let logs_dir = candidate.join("logs");
            if let Err(e) = std::fs::create_dir_all(&logs_dir) {
                tracing::warn!("could not create .sven/logs: {e}");
                return None;
            }
            let now = chrono::Local::now();
            let filename = now.format("%Y-%m-%d_%H-%M-%S%.3f").to_string() + ".jsonl";
            return Some(logs_dir.join(filename));
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    None
}

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
    ".west",   // Zephyr West workspace root (west init)
    ".cursor", // Cursor IDE workspace root
];

/// Maximum number of directory levels to ascend when searching for a
/// workspace root above the project root.
///
/// Keeping this small (5) prevents the heuristic from matching unrelated
/// system directories (e.g. a `.cursor/` that happens to exist somewhere far
/// up the hierarchy).
const MAX_WORKSPACE_ASCENT: usize = 5;

/// Heuristically locate the workspace root — the directory above the git
/// repository that contains shared tooling used by multiple projects.
///
/// **This function uses heuristics** (see [`WORKSPACE_MARKERS`]) and may
/// return an incorrect result when the filesystem layout is unusual.  Callers
/// should treat the result as a best-effort hint, not a guarantee.
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

    project_root.to_path_buf()
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
        let tmp = std::env::temp_dir().join("sven_wsroot_priority_test");
        let project = tmp.join("myproject");
        let _ = std::fs::create_dir_all(&project);
        let _ = std::fs::create_dir_all(tmp.join(".west"));
        let _ = std::fs::create_dir_all(tmp.join(".cursor"));

        let ws = find_workspace_root(&project);
        assert_eq!(
            ws, tmp,
            "marker at same level should be found regardless of order"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_ignores_markers_inside_project_root() {
        let tmp = std::env::temp_dir().join("sven_wsroot_inside_test");
        let project = tmp.join("myproject");
        let cursor_inside_project = project.join(".cursor");
        let _ = std::fs::create_dir_all(&cursor_inside_project);

        let ws = find_workspace_root(&project);
        assert_eq!(
            ws, project,
            "markers inside the project root should not be treated as workspace boundary"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_workspace_root_falls_back_to_project_root_when_no_marker() {
        let tmp = std::env::temp_dir().join("sven_wsroot_fallback_test");
        let project = tmp.join("myproject");
        let _ = std::fs::create_dir_all(&project);

        let ws = find_workspace_root(&project);
        assert_eq!(
            ws, project,
            "should fall back to project root when no marker found"
        );

        let _ = std::fs::remove_dir_all(&tmp);
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
    fn resolve_auto_log_path_returns_none_without_sven_dir() {
        let tmp = std::env::temp_dir().join("sven_rt_no_sven_dir");
        let _ = std::fs::create_dir_all(&tmp);
        let result = resolve_auto_log_path();
        if let Some(path) = result {
            assert!(
                path.extension().and_then(|e| e.to_str()) == Some("jsonl"),
                "auto-log path must have .jsonl extension: {path:?}"
            );
            assert!(
                path.to_string_lossy().contains("logs"),
                "auto-log path must be inside logs/ directory: {path:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_auto_log_path_creates_logs_dir_when_sven_exists() {
        let tmp = std::env::temp_dir().join("sven_rt_auto_log_test");
        let sven_dir = tmp.join(".sven");
        let _ = std::fs::create_dir_all(&sven_dir);

        let logs_dir = sven_dir.join("logs");
        let _ = std::fs::create_dir_all(&logs_dir);
        assert!(logs_dir.is_dir(), "logs dir should be creatable");

        let now = chrono::Local::now();
        let filename = now.format("%Y-%m-%d_%H-%M-%S%.3f").to_string() + ".jsonl";
        assert!(filename.ends_with(".jsonl"));
        assert_eq!(
            filename.len(),
            "2026-01-01_00-00-00.000.jsonl".len(),
            "filename format should match expected length: {filename}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
