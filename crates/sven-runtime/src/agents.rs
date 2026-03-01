// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Subagent discovery and shared types.
//!
//! Subagents are specialized AI assistants defined as markdown files with YAML
//! frontmatter.  The running agent reads their descriptions to decide when to
//! suggest delegation; users can invoke them explicitly with a slash command
//! (e.g. `/security-auditor check the payment module`).
//!
//! ## File locations
//!
//! Uses the same ancestor-walk strategy as skill discovery.  At every directory
//! in the merged chain, five config dirs are checked in order (lowest → highest
//! precedence within the same level):
//!
//! ```text
//! <dir>/.agents/agents/
//! <dir>/.claude/agents/
//! <dir>/.codex/agents/
//! <dir>/.cursor/agents/   ← primary Cursor location
//! <dir>/.sven/agents/     ← highest precedence
//! ```
//!
//! User-global agents at `~/.cursor/agents/` etc. are discovered through the
//! home-directory ancestor chain.
//!
//! ## File format
//!
//! Each subagent is a markdown file with optional YAML frontmatter:
//!
//! ```markdown
//! ---
//! name: security-auditor
//! description: Security specialist. Use when implementing auth or handling sensitive data.
//! model: fast
//! readonly: false
//! is_background: false
//! ---
//!
//! You are a security expert auditing code for vulnerabilities.
//! ```
//!
//! | Field           | Required | Description                                               |
//! |:----------------|:---------|:----------------------------------------------------------|
//! | `name`          | No       | Unique identifier. Defaults to filename stem.             |
//! | `description`   | No       | When to use this subagent. Defaults to first body line.   |
//! | `model`         | No       | `fast`, `inherit`, or a specific model ID.                |
//! | `readonly`      | No       | If `true`, restricted write permissions.                  |
//! | `is_background` | No       | If `true`, runs in background without waiting.            |

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use tracing::warn;

use crate::shared::Shared;
use crate::skills::{build_sorted_search_dirs, enumerate_md_files_recursive, MAX_SKILL_FILE_BYTES};

// ── Public types ──────────────────────────────────────────────────────────────

/// Information about a discovered subagent.
#[derive(Clone, Debug)]
pub struct AgentInfo {
    /// Unique name used for slash-command invocation (e.g. `"security-auditor"`).
    pub name: String,
    /// Human-readable description that guides automatic delegation.
    pub description: String,
    /// Model override: `"fast"`, `"inherit"`, or a specific model ID.
    ///
    /// `None` or `"inherit"` means: use the current session model.
    pub model: Option<String>,
    /// When `true`, the subagent runs with restricted write permissions.
    pub readonly: bool,
    /// When `true`, the subagent runs in the background without blocking.
    pub is_background: bool,
    /// System prompt body (everything after the closing `---` fence).
    pub content: String,
    /// Absolute path to the agent markdown file.
    pub agent_md_path: std::path::PathBuf,
    /// Knowledge document filenames cross-referenced by this agent spec.
    ///
    /// When set, `load_skill` appends a hint pointing the model to these
    /// knowledge docs.  Files are resolved from `.sven/knowledge/`.
    pub knowledge: Vec<String>,
}

/// A shared, live-refreshable collection of discovered subagents.
///
/// Both the TUI command registry and the running agent hold a clone of the same
/// `SharedAgents` instance.  Calling [`SharedAgents::refresh`] atomically
/// replaces the inner slice so the next turn and the next TUI command lookup
/// both see updated agents without restarting.
pub type SharedAgents = Shared<AgentInfo>;

impl Shared<AgentInfo> {
    /// Re-run agent discovery and atomically replace the agent list.
    ///
    /// Callers (e.g. the `/refresh` slash command) should also rebuild any
    /// derived state such as TUI slash commands after calling this.
    pub fn refresh(&self, project_root: Option<&Path>) {
        self.set(discover_agents(project_root));
    }
}

// ── Frontmatter schema ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    readonly: bool,
    #[serde(default)]
    is_background: bool,
    /// Knowledge doc filenames this agent cross-references.
    #[serde(default)]
    knowledge: Vec<String>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse a raw agent markdown file into an [`AgentInfo`].
///
/// If the file has no YAML frontmatter the whole body is used as the content
/// and the description is synthesised from the first non-empty line.
fn parse_agent_file(raw: &str, stem: &str, path: &std::path::Path) -> Option<AgentInfo> {
    let rest = raw.trim_start_matches('\n');

    let (fm, content) = if let Some(after_open) = rest.strip_prefix("---") {
        let close = after_open.find("\n---")?;
        let yaml_block = &after_open[..close];
        let body = after_open[close + 4..].trim_start_matches('\n').to_string();

        let fm: AgentFrontmatter = match serde_yaml::from_str(yaml_block) {
            Ok(f) => f,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to parse agent frontmatter — skipping");
                return None;
            }
        };
        (fm, body)
    } else {
        // No frontmatter: entire file is the system prompt body.
        (
            AgentFrontmatter {
                name: None,
                description: None,
                model: None,
                readonly: false,
                is_background: false,
                knowledge: vec![],
            },
            rest.to_string(),
        )
    };

    // Synthesise description from the first non-empty body line when absent.
    let description = fm
        .description
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| {
            content
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(stem)
                .trim_start_matches('#')
                .trim()
                .to_string()
        });

    if description.trim().is_empty() {
        return None;
    }

    let name = fm
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| stem.to_string());

    // Normalise model: treat "inherit" the same as absent.
    let model = fm
        .model
        .filter(|m| !m.trim().is_empty() && m.trim() != "inherit");

    // Append knowledge hint to the content body so it is visible whenever
    // the agent spec is loaded (slash command, task invocation, etc.).
    let content = if fm.knowledge.is_empty() {
        content
    } else {
        let files: Vec<String> = fm
            .knowledge
            .iter()
            .map(|f| format!("  - .sven/knowledge/{f}"))
            .collect();
        format!(
            "{content}\n\n---\n\
             **Relevant knowledge docs** — call `search_knowledge \"<topic>\"` or \
             `read_file` to load:\n{}",
            files.join("\n")
        )
    };

    Some(AgentInfo {
        name,
        description,
        model,
        readonly: fm.readonly,
        is_background: fm.is_background,
        content,
        agent_md_path: path.to_path_buf(),
        knowledge: fm.knowledge,
    })
}

/// Try to load a single agent markdown file.
fn try_load_agent(path: &std::path::Path, source: &str) -> Option<AgentInfo> {
    let size = path.metadata().map(|m| m.len()).unwrap_or(0);
    if size > MAX_SKILL_FILE_BYTES {
        warn!(
            source,
            path = %path.display(),
            size,
            max = MAX_SKILL_FILE_BYTES,
            "skipping oversized agent file"
        );
        return None;
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            warn!(source, path = %path.display(), error = %e, "failed to read agent file");
            return None;
        }
    };

    if raw.trim().is_empty() {
        return None;
    }

    parse_agent_file(&raw, stem, path)
}

fn scan_agents_dir(dir: &std::path::Path, source: &str) -> Vec<(String, AgentInfo)> {
    enumerate_md_files_recursive(dir, dir)
        .into_iter()
        .filter_map(|(key, path)| try_load_agent(&path, source).map(|a| (key, a)))
        .collect()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Discover subagents from the standard `agents/` directories.
///
/// Uses the same ancestor-walk strategy as [`discover_skills`][crate::discover_skills]
/// but scans `agents/` subdirectories instead of `skills/`.
///
/// Scanned config directories (lowest to highest precedence within a level):
///
/// ```text
/// <dir>/.agents/agents/
/// <dir>/.claude/agents/
/// <dir>/.codex/agents/
/// <dir>/.cursor/agents/   ← primary Cursor location
/// <dir>/.sven/agents/     ← highest precedence
/// ```
///
/// When `project_root` is `None`, the current working directory is used.
#[must_use]
pub fn discover_agents(project_root: Option<&Path>) -> Vec<AgentInfo> {
    let mut map: HashMap<String, AgentInfo> = HashMap::new();

    let mut load = |dir: std::path::PathBuf, source: &str| {
        for (key, agent) in scan_agents_dir(&dir, source) {
            map.insert(key, agent);
        }
    };

    for dir in &build_sorted_search_dirs(project_root) {
        let label = dir.to_string_lossy();
        load(
            dir.join(".agents").join("agents"),
            &format!("{label}/.agents"),
        );
        load(
            dir.join(".claude").join("agents"),
            &format!("{label}/.claude"),
        );
        load(
            dir.join(".codex").join("agents"),
            &format!("{label}/.codex"),
        );
        load(
            dir.join(".cursor").join("agents"),
            &format!("{label}/.cursor"),
        );
        load(dir.join(".sven").join("agents"), &format!("{label}/.sven"));
    }

    let mut result: Vec<AgentInfo> = map.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_agent(
        dir: &std::path::Path,
        name: &str,
        description: &str,
        extra_fm: &str,
        body: &str,
    ) {
        fs::create_dir_all(dir).unwrap();
        let content = format!("---\ndescription: {description}\n{extra_fm}---\n\n{body}");
        fs::write(dir.join(format!("{name}.md")), content).unwrap();
    }

    #[test]
    fn parse_agent_file_valid() {
        let raw = "---\ndescription: A test agent.\n---\n\nYou are a test assistant.";
        let path = std::path::PathBuf::from("/tmp/test-agent.md");
        let info = parse_agent_file(raw, "test-agent", &path).expect("should parse");
        assert_eq!(info.name, "test-agent");
        assert_eq!(info.description.trim(), "A test agent.");
        assert_eq!(info.content.trim(), "You are a test assistant.");
        assert!(info.model.is_none());
        assert!(!info.readonly);
        assert!(!info.is_background);
    }

    #[test]
    fn parse_agent_file_with_name_and_model() {
        let raw = "---\nname: security-auditor\ndescription: Security specialist.\nmodel: fast\nreadonly: true\n---\n\nAudit body.";
        let path = std::path::PathBuf::from("/tmp/security-auditor.md");
        let info = parse_agent_file(raw, "security-auditor", &path).expect("should parse");
        assert_eq!(info.name, "security-auditor");
        assert_eq!(info.model.as_deref(), Some("fast"));
        assert!(info.readonly);
    }

    #[test]
    fn parse_agent_file_model_inherit_becomes_none() {
        let raw = "---\ndescription: Test.\nmodel: inherit\n---\n\nBody.";
        let path = std::path::PathBuf::from("/tmp/test.md");
        let info = parse_agent_file(raw, "test", &path).expect("should parse");
        assert!(info.model.is_none(), "inherit should normalise to None");
    }

    #[test]
    fn parse_agent_file_no_frontmatter() {
        let raw = "# You are a specialist.\n\nHelp with tasks.";
        let path = std::path::PathBuf::from("/tmp/agent.md");
        let info =
            parse_agent_file(raw, "agent", &path).expect("no-frontmatter agent should parse");
        assert_eq!(info.name, "agent");
        assert!(!info.description.is_empty());
    }

    #[test]
    fn discover_agents_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let result = discover_agents(Some(tmp.path()));
        assert!(result.is_empty());
    }

    #[test]
    fn discover_agents_cursor_location() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".cursor").join("agents");
        write_agent(
            &agents_dir,
            "verifier",
            "Validates completed work.",
            "",
            "You verify things.",
        );

        let agents = discover_agents(Some(tmp.path()));
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "verifier");
        assert!(agents[0].description.contains("Validates completed work."));
    }

    #[test]
    fn discover_agents_sven_overrides_cursor() {
        let tmp = TempDir::new().unwrap();
        write_agent(
            &tmp.path().join(".cursor").join("agents"),
            "verifier",
            "Cursor version.",
            "",
            "Cursor body.",
        );
        write_agent(
            &tmp.path().join(".sven").join("agents"),
            "verifier",
            "Sven version.",
            "",
            "Sven body.",
        );

        let agents = discover_agents(Some(tmp.path()));
        assert_eq!(agents.len(), 1);
        assert!(agents[0].description.contains("Sven version."));
    }

    #[test]
    fn discover_agents_multiple_sorted_by_name() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".cursor").join("agents");
        write_agent(&dir, "zebra", "Z agent.", "", "");
        write_agent(&dir, "apple", "A agent.", "", "");
        write_agent(&dir, "security", "S agent.", "", "");

        let agents = discover_agents(Some(tmp.path()));
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0].name, "apple");
        assert_eq!(agents[1].name, "security");
        assert_eq!(agents[2].name, "zebra");
    }

    #[test]
    fn discover_agents_size_cap_skips_oversized() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".cursor").join("agents");
        fs::create_dir_all(&dir).unwrap();
        let big_content = format!(
            "---\ndescription: Oversized.\n---\n\n{}",
            "x".repeat(260 * 1024)
        );
        fs::write(dir.join("big-agent.md"), big_content).unwrap();

        let agents = discover_agents(Some(tmp.path()));
        assert!(agents.is_empty(), "oversized agent should be skipped");
    }
}
