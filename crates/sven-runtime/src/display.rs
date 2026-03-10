// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Pure formatting functions for human-readable display of runtime objects.
//!
//! These functions produce markdown strings with no dependency on ratatui or
//! any TUI library.  They can be used in tests, CI output, and the TUI alike.

use std::collections::BTreeMap;

use crate::{AgentInfo, SkillInfo};

// ── Generic grouped-list formatter ────────────────────────────────────────────

/// Format a slice of items as a grouped markdown list.
///
/// Items are grouped by the string returned by `group_fn`, then sorted
/// alphabetically within each group.  `format_entry` produces the markdown
/// text for each item (should end with `\n\n`).  The section header uses
/// `title` and the total count.
///
/// This is the shared engine behind `format_skills_tree`, `format_agents_list`,
/// and `format_tools_list` (in `sven-tools`) — all three share the same
/// `BTreeMap` grouping + `## Title (N total)\n### ns\n` skeleton.
pub fn format_grouped_list<T, G, F>(
    items: &[T],
    title: &str,
    empty_label: &str,
    group_fn: G,
    sort_key_fn: impl Fn(&T) -> String,
    format_entry: F,
) -> String
where
    G: Fn(&T) -> String,
    F: Fn(&T) -> String,
{
    if items.is_empty() {
        return format!("## {title}\n\n_{empty_label}_\n");
    }

    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        groups.entry(group_fn(item)).or_default().push(i);
    }

    let mut out = format!("## {title} ({} total)\n", items.len());

    for (ns, mut indices) in groups {
        indices.sort_by_key(|&i| sort_key_fn(&items[i]));
        out.push_str(&format!("\n### {ns}\n\n"));
        for i in indices {
            out.push_str(&format_entry(&items[i]));
        }
    }

    out
}

// ── Skills ────────────────────────────────────────────────────────────────────

/// Format a slice of [`SkillInfo`] as a hierarchical markdown tree.
///
/// Skills are grouped by the top-level segment of their `command` field
/// (the part before the first `/`).  Within each group they are sorted
/// alphabetically.  Each entry shows the command, description, version,
/// full absolute path (clickable in Cursor terminal), and any sven-specific
/// flags.
///
/// # Example output
///
/// ```text
/// ## Skills (5 total)
///
/// ### git
///
/// **git/commit** — Commit staged changes following project conventions
/// `v1.0`  [always]
/// /home/user/.cursor/skills/git/commit/SKILL.md
///
/// ### sven
///
/// **sven/plan** — Plan a development task
/// /data/.cursor/skills/sven/plan/SKILL.md
/// ```
pub fn format_skills_tree(skills: &[SkillInfo]) -> String {
    format_grouped_list(
        skills,
        "Skills",
        "No skills discovered.",
        |s| {
            s.command
                .split('/')
                .next()
                .unwrap_or(&s.command)
                .to_string()
        },
        |s| s.command.clone(),
        |skill| {
            let mut entry = format!("**{}**", skill.command);
            if !skill.description.is_empty() {
                entry.push_str(&format!(" — {}", skill.description.trim()));
            }
            entry.push('\n');

            let mut meta: Vec<String> = Vec::new();
            if let Some(ref v) = skill.version {
                meta.push(format!("`{v}`"));
            }
            if let Some(ref sm) = skill.sven_meta {
                if sm.always {
                    meta.push("[always]".to_string());
                }
                if sm.user_invocable_only {
                    meta.push("[user-only]".to_string());
                }
                if !sm.requires_bins.is_empty() {
                    meta.push(format!("[requires: {}]", sm.requires_bins.join(", ")));
                }
                if !sm.requires_env.is_empty() {
                    meta.push(format!("[env: {}]", sm.requires_env.join(", ")));
                }
            }
            if !meta.is_empty() {
                entry.push_str(&format!("{}  \n", meta.join("  ")));
            }

            entry.push_str(&format!("{}\n\n", skill.skill_md_path.display()));
            entry
        },
    )
}

// ── Agents ────────────────────────────────────────────────────────────────────

/// Format a slice of [`AgentInfo`] as a markdown list.
///
/// Each entry shows the name (used as slash command), description, model
/// override, flags, knowledge docs, and the full absolute path to the agent
/// markdown file.
///
/// # Example output
///
/// ```text
/// ## Subagents (2 total)
///
/// **security-auditor** — Security specialist. Use when implementing auth.
/// Model: fast  [readonly]
/// /data/.cursor/agents/security-auditor.md
/// ```
pub fn format_agents_list(agents: &[AgentInfo]) -> String {
    // Agents are not grouped by namespace — use a single flat group.
    format_grouped_list(
        agents,
        "Subagents",
        "No subagents discovered.",
        |_| "all".to_string(),
        |a| a.name.clone(),
        |agent| {
            let mut entry = format!("**{}**", agent.name);
            if !agent.description.is_empty() {
                entry.push_str(&format!(" — {}", agent.description.trim()));
            }
            entry.push('\n');

            let mut meta: Vec<String> = Vec::new();
            if let Some(ref model) = agent.model {
                meta.push(format!("Model: {model}"));
            }
            if agent.readonly {
                meta.push("[readonly]".to_string());
            }
            if agent.is_background {
                meta.push("[background]".to_string());
            }
            if !meta.is_empty() {
                entry.push_str(&format!("{}  \n", meta.join("  ")));
            }

            if !agent.knowledge.is_empty() {
                entry.push_str(&format!("Knowledge: {}  \n", agent.knowledge.join(", ")));
            }

            entry.push_str(&format!("{}\n\n", agent.agent_md_path.display()));
            entry
        },
    )
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{AgentInfo, SkillInfo};

    fn make_skill(command: &str, description: &str, path: &str) -> SkillInfo {
        SkillInfo {
            command: command.to_string(),
            name: command
                .split('/')
                .next_back()
                .unwrap_or(command)
                .to_string(),
            description: description.to_string(),
            version: None,
            skill_md_path: PathBuf::from(path),
            skill_dir: PathBuf::from(path).parent().unwrap().to_path_buf(),
            content: String::new(),
            sven_meta: None,
        }
    }

    fn make_agent(name: &str, description: &str, path: &str) -> AgentInfo {
        AgentInfo {
            name: name.to_string(),
            description: description.to_string(),
            model: None,
            readonly: false,
            is_background: false,
            content: String::new(),
            agent_md_path: PathBuf::from(path),
            knowledge: vec![],
        }
    }

    #[test]
    fn empty_skills_returns_placeholder() {
        let out = format_skills_tree(&[]);
        assert!(out.contains("No skills discovered"));
    }

    #[test]
    fn skills_grouped_by_namespace() {
        let skills = vec![
            make_skill(
                "sven/plan",
                "Plan tasks",
                "/p/.cursor/skills/sven/plan/SKILL.md",
            ),
            make_skill(
                "sven/implement",
                "Implement",
                "/p/.cursor/skills/sven/implement/SKILL.md",
            ),
            make_skill(
                "git/commit",
                "Commit",
                "/p/.cursor/skills/git/commit/SKILL.md",
            ),
        ];
        let out = format_skills_tree(&skills);
        assert!(out.contains("### sven"));
        assert!(out.contains("### git"));
        assert!(out.contains("**sven/plan**"));
        assert!(out.contains("**git/commit**"));
        assert!(out.contains("3 total"));
    }

    #[test]
    fn skill_with_version_and_flags() {
        let mut skill = make_skill(
            "my/skill",
            "Does stuff",
            "/p/.sven/skills/my/skill/SKILL.md",
        );
        skill.version = Some("1.2.3".to_string());
        skill.sven_meta = Some(crate::SvenSkillMeta {
            always: true,
            user_invocable_only: false,
            requires_bins: vec![],
            requires_env: vec![],
        });
        let out = format_skills_tree(&[skill]);
        assert!(out.contains("`1.2.3`"));
        assert!(out.contains("[always]"));
    }

    #[test]
    fn skill_path_shown() {
        let skill = make_skill(
            "git/commit",
            "Commit",
            "/home/user/.cursor/skills/git/commit/SKILL.md",
        );
        let out = format_skills_tree(&[skill]);
        assert!(out.contains("/home/user/.cursor/skills/git/commit/SKILL.md"));
    }

    #[test]
    fn empty_agents_returns_placeholder() {
        let out = format_agents_list(&[]);
        assert!(out.contains("No subagents discovered"));
    }

    #[test]
    fn agents_list_shows_name_description_path() {
        let agents = vec![
            make_agent(
                "security-auditor",
                "Audits code",
                "/p/.cursor/agents/security-auditor.md",
            ),
            make_agent("verifier", "Verifies work", "/p/.cursor/agents/verifier.md"),
        ];
        let out = format_agents_list(&agents);
        assert!(out.contains("**security-auditor**"));
        assert!(out.contains("Audits code"));
        assert!(out.contains("/p/.cursor/agents/security-auditor.md"));
        assert!(out.contains("2 total"));
    }

    #[test]
    fn agent_with_model_and_flags() {
        let mut agent = make_agent(
            "fast-helper",
            "Helps fast",
            "/p/.cursor/agents/fast-helper.md",
        );
        agent.model = Some("fast".to_string());
        agent.readonly = true;
        agent.is_background = true;
        agent.knowledge = vec!["api-docs.md".to_string()];
        let out = format_agents_list(&[agent]);
        assert!(out.contains("Model: fast"));
        assert!(out.contains("[readonly]"));
        assert!(out.contains("[background]"));
        assert!(out.contains("Knowledge: api-docs.md"));
    }
}
