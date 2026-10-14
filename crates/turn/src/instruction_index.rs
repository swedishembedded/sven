// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The compact indexes of deferred disclosure: skills, subagents and
//! knowledge documents.
//!
//! Each skill is one line, its command and a summary of at most
//! [`SUMMARY_CHARS`](crate::instructions::SUMMARY_CHARS) characters. Above
//! [`INDEX_LINE_LIMIT`] skills the index gives the number of skills in each
//! package instead, so a project with hundreds of skills does not pay for
//! hundreds of lines on every request. A pinned skill (`sven.always`) keeps
//! its full entry whatever the mode, and a skill the user may only invoke is
//! left out of the model's listing, as in the full listing.

use std::collections::BTreeMap;

use sven_workspace::{AgentInfo, KnowledgeInfo, SkillInfo};

use crate::instructions::{summary, SUMMARY_CHARS};

/// The most skills the index names one by one.
pub const INDEX_LINE_LIMIT: usize = 50;

/// Phrases that open many descriptions and say nothing about the skill.
const BOILERPLATE: &[&str] = &[
    "this skill should be used when ",
    "use this skill when ",
    "use this subagent when ",
    "use this agent when ",
    "use when ",
];

fn entry(skill: &SkillInfo) -> String {
    format!(
        "  <skill>\n    <command>{}</command>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
        skill.command,
        skill.name,
        skill.description.trim()
    )
}

fn short(description: &str) -> String {
    let line = summary(description, usize::MAX);
    let lower = line.to_lowercase();
    let trimmed = BOILERPLATE
        .iter()
        .find(|b| lower.starts_with(**b))
        .map_or(line.as_str(), |b| &line[b.len()..]);
    summary(trimmed, SUMMARY_CHARS)
}

/// The `## Skills` section for `skills`, which are the ones the system prompt
/// lists (the caller has already left out conditional skills). Empty when
/// there is nothing to list.
#[must_use]
pub fn build_skills_index(skills: &[SkillInfo]) -> String {
    let pinned: Vec<&SkillInfo> = skills
        .iter()
        .filter(|s| s.sven_meta.as_ref().is_some_and(|m| m.always))
        .collect();
    let indexed: Vec<&SkillInfo> = skills
        .iter()
        .filter(|s| {
            s.sven_meta
                .as_ref()
                .is_none_or(|m| !m.always && !m.user_invocable_only)
        })
        .collect();
    if pinned.is_empty() && indexed.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = pinned.iter().map(|s| entry(s)).collect();
    if indexed.len() > INDEX_LINE_LIMIT {
        let mut packages: BTreeMap<&str, usize> = BTreeMap::new();
        for skill in &indexed {
            *packages
                .entry(skill.command.split('/').next().unwrap_or(&skill.command))
                .or_default() += 1;
        }
        lines.push(format!(
            "{} more skills, by package (`search` finds one, `list` with a `regex` narrows by package):",
            indexed.len()
        ));
        lines.extend(packages.iter().map(|(name, n)| format!("- {name}: {n}")));
    } else {
        lines.extend(
            indexed
                .iter()
                .map(|s| format!("- {}: {}", s.command, short(&s.description))),
        );
    }

    format!(
        "## Skills\n\n\
         When the current task matches a skill below, call the `skill` tool with \
         `action: \"load\"` and the skill's command as `name` to load its instructions \
         before proceeding. The index gives each skill's command and a short summary; to \
         look for a skill that is not listed or whose summary is unclear, call `skill` \
         with `action: \"search\"` and a `query`. Load at most one skill per task; do \
         not load a skill unless it clearly applies.\n\n\
         <available_skills>\n{}\n</available_skills>",
        lines.join("\n")
    )
}

/// The `## Subagents` section: a name and a summary per subagent, a count above
/// [`INDEX_LINE_LIMIT`]. `agents` are the ones the system prompt lists.
#[must_use]
pub fn build_agents_index(agents: &[AgentInfo]) -> String {
    if agents.is_empty() {
        return String::new();
    }
    let body = if agents.len() > INDEX_LINE_LIMIT {
        format!(
            "{} subagents; `action: \"agents\"` lists them.",
            agents.len()
        )
    } else {
        agents
            .iter()
            .map(|a| {
                let readonly = if a.readonly { " (readonly)" } else { "" };
                format!("- {}{readonly}: {}", a.name, short(&a.description))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "## Subagents\n\n\
         The following subagents are available for delegation. Delegate with the `task` \
         tool, passing the subagent's name as `mode`; when the user's task clearly matches \
         one, suggest it as a slash command (e.g. `/verifier confirm the auth flow`). \
         The index gives a short summary of each; `task` with `action: \"agents\"` returns \
         the full descriptions.\n\n\
         <available_agents>\n{body}\n</available_agents>"
    )
}

/// The `## Knowledge Base` section: a subsystem and the files it covers per
/// document, a count above [`INDEX_LINE_LIMIT`]. `knowledge` are the documents
/// the system prompt lists.
#[must_use]
pub fn build_knowledge_index(knowledge: &[KnowledgeInfo]) -> String {
    if knowledge.is_empty() {
        return String::new();
    }
    let body = if knowledge.len() > INDEX_LINE_LIMIT {
        format!(
            "{} documents; `search_knowledge \"<topic>\"` finds the ones for a topic and \
             `list_knowledge` shows all.",
            knowledge.len()
        )
    } else {
        knowledge
            .iter()
            .map(|k| {
                if k.files.is_empty() {
                    format!("- {}", k.subsystem)
                } else {
                    format!("- {} ({})", k.subsystem, k.files.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "## Knowledge Base\n\n\
         Project knowledge documents are available for these subsystems, with the files \
         each covers. Use `search_knowledge \"<topic>\"` to find relevant specs before \
         modifying a subsystem, or `list_knowledge` to see all documents.\n\n\
         <knowledge_base>\n{body}\n</knowledge_base>"
    )
}
