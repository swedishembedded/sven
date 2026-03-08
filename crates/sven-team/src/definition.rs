// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Team definition files — `.sven/teams/*.yaml`.
//!
//! A team definition file describes a named team: its goal, member roles, per-member
//! model overrides, and per-member instruction fragments.  It is the declarative
//! alternative to constructing a team entirely through LLM tool calls.
//!
//! ## Example
//!
//! ```yaml
//! name: code-review
//! goal: Review PR changes for correctness and style
//! members:
//!   - role: reviewer
//!     name: security-reviewer
//!     model: anthropic/claude-sonnet-4-5
//!     instructions: Focus on authentication, authorization, and input validation.
//!   - role: reviewer
//!     name: perf-reviewer
//!     model: anthropic/claude-sonnet-4-5
//!     instructions: Focus on algorithmic complexity and resource usage.
//!   - role: tester
//!     name: test-writer
//!     model: openai/gpt-5.2
//!     instructions: Write missing test cases for every changed function.
//! max_active: 4
//! token_budget: 200000
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::TeamRole;

// ── TeamMemberDef ─────────────────────────────────────────────────────────────

/// One member entry in a team definition file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberDef {
    /// Role within the team (implementer, reviewer, explorer, tester, teammate).
    pub role: TeamRole,
    /// Human-readable name for this agent.
    pub name: String,
    /// LLM model override (e.g. `"anthropic/claude-sonnet-4-5"`).
    /// When omitted the team lead's model is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Short instructions appended to the teammate's system prompt.
    /// Use this to specialise a generic role (e.g. "Focus on SQL queries").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Tool names that this member is NOT allowed to call.
    /// Useful for making a reviewer read-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
}

// ── TeamDefinition ────────────────────────────────────────────────────────────

/// A complete team definition loaded from a `.sven/teams/*.yaml` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamDefinition {
    /// Team name — also used as the directory name under `~/.config/sven/teams/`.
    pub name: String,
    /// Description of the team's overall objective (shown in `sven team status`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Member specifications.
    #[serde(default)]
    pub members: Vec<TeamMemberDef>,
    /// Maximum simultaneous active teammates (default: 8).
    #[serde(default = "default_max_active")]
    pub max_active: usize,
    /// Global token budget across all team members (0 = unlimited).
    #[serde(default)]
    pub token_budget: u64,
    /// Maximum agent-loop iterations per teammate task (0 = unlimited).
    #[serde(default)]
    pub max_iterations: u32,
}

fn default_max_active() -> usize {
    8
}

impl TeamDefinition {
    /// Load a team definition from a YAML file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read team definition {:?}: {e}", path))?;
        let def: TeamDefinition = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid team definition {:?}: {e}", path))?;
        Ok(def)
    }

    /// Save the definition to a YAML file (pretty-printed).
    pub fn to_file(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_yaml::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize team definition: {e}"))?;
        std::fs::write(path, content)
            .map_err(|e| anyhow::anyhow!("Failed to write team definition {:?}: {e}", path))?;
        Ok(())
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Discover all team definition files in `.sven/teams/` under the given root.
///
/// Returns a list of `(path, definition)` pairs.
pub fn discover_team_definitions(
    project_root: &Path,
) -> anyhow::Result<Vec<(PathBuf, TeamDefinition)>> {
    let dir = project_root.join(".sven").join("teams");
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml")
            || path.extension().and_then(|e| e.to_str()) == Some("yml")
        {
            match TeamDefinition::from_file(&path) {
                Ok(def) => results.push((path, def)),
                Err(e) => eprintln!(
                    "[sven:warn] Skipping invalid team file {}: {e}",
                    path.display()
                ),
            }
        }
    }

    results.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(results)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn example_def() -> TeamDefinition {
        TeamDefinition {
            name: "code-review".to_string(),
            goal: Some("Review PR changes".to_string()),
            members: vec![
                TeamMemberDef {
                    role: TeamRole::Reviewer,
                    name: "security-reviewer".to_string(),
                    model: Some("anthropic/claude-sonnet-4-5".to_string()),
                    instructions: Some("Focus on auth and input validation.".to_string()),
                    deny_tools: vec!["write_file".to_string(), "edit_file".to_string()],
                },
                TeamMemberDef {
                    role: TeamRole::Tester,
                    name: "test-writer".to_string(),
                    model: None,
                    instructions: None,
                    deny_tools: Vec::new(),
                },
            ],
            max_active: 4,
            token_budget: 200_000,
            max_iterations: 0,
        }
    }

    #[test]
    fn roundtrip_yaml() {
        let def = example_def();
        let yaml = serde_yaml::to_string(&def).unwrap();
        let loaded: TeamDefinition = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(loaded.name, def.name);
        assert_eq!(loaded.members.len(), 2);
        assert_eq!(
            loaded.members[0].deny_tools,
            vec!["write_file", "edit_file"]
        );
    }

    #[test]
    fn file_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("review.yaml");
        let def = example_def();
        def.to_file(&path).unwrap();
        let loaded = TeamDefinition::from_file(&path).unwrap();
        assert_eq!(loaded.name, "code-review");
        assert_eq!(loaded.goal.unwrap(), "Review PR changes");
        assert_eq!(loaded.token_budget, 200_000);
    }

    #[test]
    fn discover_returns_empty_for_missing_dir() {
        let dir = TempDir::new().unwrap();
        let results = discover_team_definitions(dir.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn discover_finds_yaml_files() {
        let dir = TempDir::new().unwrap();
        let teams_dir = dir.path().join(".sven").join("teams");
        std::fs::create_dir_all(&teams_dir).unwrap();
        let def = example_def();
        def.to_file(&teams_dir.join("review.yaml")).unwrap();
        let results = discover_team_definitions(dir.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.name, "code-review");
    }
}
