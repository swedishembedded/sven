// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Knowledge base discovery, parsing, and drift detection.
//!
//! Knowledge documents are plain Markdown files with YAML frontmatter stored
//! under `.sven/knowledge/` in the project root.  Each file documents one
//! subsystem and is written for AI consumption — explicit file paths, code
//! patterns, correctness invariants, and known failure modes.
//!
//! ## File format
//!
//! ```markdown
//! ---
//! subsystem: P2P Networking
//! files:
//!   - crates/sven-p2p/**
//!   - crates/sven-node/**
//! updated: 2026-03-01
//! ---
//!
//! ## Core Architecture
//! ...
//!
//! ## Known Failure Modes
//! | Symptom | Cause | Fix |
//! ```
//!
//! | Field       | Required | Description                                           |
//! |:------------|:---------|:------------------------------------------------------|
//! | `subsystem` | Yes      | Human-readable name shown in tool output              |
//! | `files`     | No       | Glob patterns for files this doc covers               |
//! | `updated`   | No       | ISO date (YYYY-MM-DD) when doc was last reviewed      |
//!
//! ## Drift detection
//!
//! When `updated:` is set, Sven checks at session start whether any file
//! matching a `files:` glob has been committed since that date.  If so, a
//! warning is injected into the dynamic system-prompt block, reminding the
//! agent to consult and potentially update the knowledge doc.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::warn;

use crate::shared::Shared;

// ── Public types ──────────────────────────────────────────────────────────────

/// A parsed knowledge document.
#[derive(Debug, Clone)]
pub struct KnowledgeInfo {
    /// Human-readable subsystem name (from frontmatter `subsystem:`).
    pub subsystem: String,
    /// Glob patterns for source files this document covers (from `files:`).
    pub files: Vec<String>,
    /// ISO-date string when the document was last reviewed (from `updated:`).
    /// `None` when the `updated:` field is absent — no drift detection for
    /// that document.
    pub updated: Option<String>,
    /// Absolute path to the `.md` file.
    pub path: PathBuf,
    /// Document body — everything after the closing YAML `---` fence.
    pub body: String,
}

/// Thread-safe, live-refreshable collection of discovered knowledge documents.
pub type SharedKnowledge = Shared<KnowledgeInfo>;

impl Shared<KnowledgeInfo> {
    /// Re-run knowledge discovery and atomically replace the list.
    pub fn refresh(&self, project_root: Option<&Path>) {
        self.set(discover_knowledge(project_root));
    }
}

/// A single drift warning produced when source files have been modified after
/// a knowledge document's `updated:` date.
#[derive(Debug, Clone)]
pub struct DriftWarning {
    /// Subsystem name from the knowledge document.
    pub subsystem: String,
    /// File name of the knowledge document (e.g. `"sven-p2p.md"`).
    pub knowledge_file: String,
    /// The `updated:` date recorded in the document.
    pub updated: String,
    /// Up to 3 changed file paths (for context in the warning message).
    pub changed_files: Vec<String>,
}

// ── Frontmatter schema ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct KnowledgeFrontmatter {
    subsystem: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    updated: Option<String>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Maximum bytes read from a single knowledge file.
const MAX_KNOWLEDGE_FILE_BYTES: usize = 128 * 1024;

/// Parse a raw knowledge markdown file into a [`KnowledgeInfo`].
///
/// Returns `None` if the file lacks the required `subsystem:` frontmatter
/// field, if the frontmatter is malformed, or if the body is empty after
/// stripping the frontmatter.
fn parse_knowledge_file(raw: &str, path: &Path) -> Option<KnowledgeInfo> {
    let rest = raw.trim_start_matches('\n');

    let (fm, body) = if let Some(after_open) = rest.strip_prefix("---") {
        let close = after_open.find("\n---")?;
        let yaml_block = &after_open[..close];
        let body = after_open[close + 4..].trim_start_matches('\n').to_string();

        let fm: KnowledgeFrontmatter = match serde_yaml::from_str(yaml_block) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse knowledge frontmatter — skipping"
                );
                return None;
            }
        };
        (fm, body)
    } else {
        warn!(
            path = %path.display(),
            "knowledge file has no YAML frontmatter — skipping \
             (add `---\\nsubsystem: MySystem\\n---` header)"
        );
        return None;
    };

    if fm.subsystem.trim().is_empty() {
        warn!(path = %path.display(), "knowledge file missing `subsystem:` field — skipping");
        return None;
    }

    Some(KnowledgeInfo {
        subsystem: fm.subsystem.trim().to_string(),
        files: fm.files,
        updated: fm.updated,
        path: path.to_path_buf(),
        body,
    })
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Scan `<project_root>/.sven/knowledge/*.md` and return all parseable docs.
///
/// Files are sorted by subsystem name for deterministic output.  Oversized
/// files (> 128 KiB) are skipped with a warning.
#[must_use]
pub fn discover_knowledge(project_root: Option<&Path>) -> Vec<KnowledgeInfo> {
    let Some(root) = project_root else {
        return vec![];
    };

    let knowledge_dir = root.join(".sven").join("knowledge");
    if !knowledge_dir.is_dir() {
        return vec![];
    }

    let entries = match std::fs::read_dir(&knowledge_dir) {
        Ok(e) => e,
        Err(err) => {
            warn!(
                dir = %knowledge_dir.display(),
                error = %err,
                "could not read knowledge directory"
            );
            return vec![];
        }
    };

    let mut docs: Vec<KnowledgeInfo> = entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let path = e.path();

            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            if size > MAX_KNOWLEDGE_FILE_BYTES as u64 {
                warn!(
                    path = %path.display(),
                    size,
                    max = MAX_KNOWLEDGE_FILE_BYTES,
                    "skipping oversized knowledge file"
                );
                return None;
            }

            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "failed to read knowledge file");
                    return None;
                }
            };

            if raw.trim().is_empty() {
                return None;
            }

            parse_knowledge_file(&raw, &path)
        })
        .collect();

    docs.sort_by(|a, b| a.subsystem.cmp(&b.subsystem));
    docs
}

// ── Drift detection ───────────────────────────────────────────────────────────

/// Check whether any knowledge document is stale relative to recent git commits.
///
/// For each document that has an `updated:` date and at least one `files:`
/// glob pattern, runs:
///
/// ```text
/// git log --since=<updated> --name-only --format= -- <pattern>
/// ```
///
/// If git reports changed files, a [`DriftWarning`] is produced for that
/// document.  Documents without an `updated:` field are silently skipped.
///
/// Returns an empty `Vec` when `project_root` is not in a git repository, git
/// is not available, or all documents are current.
#[must_use]
pub fn check_knowledge_drift(
    project_root: &Path,
    knowledge: &[KnowledgeInfo],
) -> Vec<DriftWarning> {
    let mut warnings = Vec::new();

    for doc in knowledge {
        let Some(updated) = &doc.updated else {
            continue;
        };

        if doc.files.is_empty() {
            continue;
        }

        let mut changed_files: Vec<String> = Vec::new();

        for pattern in &doc.files {
            let args: Vec<&str> = vec![
                "log",
                "--since",
                updated,
                "--name-only",
                "--format=",
                "--",
                pattern,
            ];
            if let Some(output) = crate::run_git_timed_pub(&args, project_root) {
                let files: Vec<String> = output
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                changed_files.extend(files);
                // Deduplicate as patterns can overlap.
                changed_files.sort();
                changed_files.dedup();
            }
        }

        if !changed_files.is_empty() {
            // Truncate to first 3 for brevity in the prompt.
            changed_files.truncate(3);
            let knowledge_file = doc
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown.md")
                .to_string();
            warnings.push(DriftWarning {
                subsystem: doc.subsystem.clone(),
                knowledge_file,
                updated: updated.clone(),
                changed_files,
            });
        }
    }

    warnings
}

/// Format drift warnings as a system-prompt section.
///
/// Returns `None` when there are no warnings.
#[must_use]
pub fn format_drift_warnings(warnings: &[DriftWarning]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }

    let mut lines = vec!["## Knowledge Drift Detected".to_string(), String::new()];

    for w in warnings {
        lines.push(format!(
            "⚠ `.sven/knowledge/{}` covers `{}` — last updated {}.",
            w.knowledge_file, w.subsystem, w.updated
        ));
        lines.push(format!(
            "  Files committed since then: {}",
            w.changed_files.join(", ")
        ));
        lines.push(format!(
            "  Before editing these files, call `search_knowledge \"{}\"` and update the doc after changes.",
            w.subsystem
        ));
        lines.push(String::new());
    }

    Some(lines.join("\n").trim_end().to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn knowledge_dir(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join(".sven").join("knowledge");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_doc(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(format!("{name}.md")), content).unwrap();
    }

    #[test]
    fn parse_knowledge_file_valid() {
        let raw = "---\nsubsystem: P2P Networking\nfiles:\n  - crates/sven-p2p/**\nupdated: 2026-01-15\n---\n\n## Core Architecture\n\nDetail here.";
        let path = PathBuf::from("/tmp/sven-p2p.md");
        let info = parse_knowledge_file(raw, &path).expect("should parse");
        assert_eq!(info.subsystem, "P2P Networking");
        assert_eq!(info.files, vec!["crates/sven-p2p/**"]);
        assert_eq!(info.updated.as_deref(), Some("2026-01-15"));
        assert!(info.body.contains("Core Architecture"));
    }

    #[test]
    fn parse_knowledge_file_no_frontmatter_returns_none() {
        let raw = "# Just a heading\n\nNo frontmatter.";
        let path = PathBuf::from("/tmp/no-fm.md");
        assert!(parse_knowledge_file(raw, &path).is_none());
    }

    #[test]
    fn parse_knowledge_file_missing_subsystem_returns_none() {
        let raw = "---\nfiles:\n  - crates/**\n---\n\nBody.";
        let path = PathBuf::from("/tmp/missing.md");
        assert!(parse_knowledge_file(raw, &path).is_none());
    }

    #[test]
    fn parse_knowledge_file_no_files_or_updated() {
        let raw = "---\nsubsystem: Config\n---\n\nMinimal doc.";
        let path = PathBuf::from("/tmp/config.md");
        let info = parse_knowledge_file(raw, &path).expect("should parse with minimal fields");
        assert_eq!(info.subsystem, "Config");
        assert!(info.files.is_empty());
        assert!(info.updated.is_none());
    }

    #[test]
    fn discover_knowledge_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let result = discover_knowledge(Some(tmp.path()));
        assert!(result.is_empty());
    }

    #[test]
    fn discover_knowledge_no_sven_dir() {
        let tmp = TempDir::new().unwrap();
        let result = discover_knowledge(Some(tmp.path()));
        assert!(
            result.is_empty(),
            "should return empty when .sven/knowledge/ does not exist"
        );
    }

    #[test]
    fn discover_knowledge_finds_valid_docs() {
        let tmp = TempDir::new().unwrap();
        let dir = knowledge_dir(&tmp);
        write_doc(
            &dir,
            "p2p",
            "---\nsubsystem: P2P\nfiles:\n  - crates/sven-p2p/**\n---\n\nP2P body.",
        );
        write_doc(&dir, "core", "---\nsubsystem: Core\n---\n\nCore body.");

        let docs = discover_knowledge(Some(tmp.path()));
        assert_eq!(docs.len(), 2);
        // sorted by subsystem
        assert_eq!(docs[0].subsystem, "Core");
        assert_eq!(docs[1].subsystem, "P2P");
    }

    #[test]
    fn discover_knowledge_skips_invalid_files() {
        let tmp = TempDir::new().unwrap();
        let dir = knowledge_dir(&tmp);
        write_doc(&dir, "valid", "---\nsubsystem: Valid\n---\n\nBody.");
        write_doc(&dir, "invalid", "# No frontmatter\n\nJust text.");

        let docs = discover_knowledge(Some(tmp.path()));
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].subsystem, "Valid");
    }

    #[test]
    fn discover_knowledge_skips_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let dir = knowledge_dir(&tmp);
        write_doc(&dir, "doc", "---\nsubsystem: Doc\n---\n\nBody.");
        fs::write(dir.join("notes.txt"), "just text").unwrap();

        let docs = discover_knowledge(Some(tmp.path()));
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn format_drift_warnings_empty_returns_none() {
        assert!(format_drift_warnings(&[]).is_none());
    }

    #[test]
    fn format_drift_warnings_single() {
        let warnings = vec![DriftWarning {
            subsystem: "P2P Networking".to_string(),
            knowledge_file: "sven-p2p.md".to_string(),
            updated: "2026-01-15".to_string(),
            changed_files: vec!["crates/sven-p2p/src/node.rs".to_string()],
        }];
        let text = format_drift_warnings(&warnings).unwrap();
        assert!(text.contains("Knowledge Drift Detected"));
        assert!(text.contains("sven-p2p.md"));
        assert!(text.contains("P2P Networking"));
        assert!(text.contains("2026-01-15"));
        assert!(text.contains("crates/sven-p2p/src/node.rs"));
        assert!(text.contains("search_knowledge"));
    }
}
