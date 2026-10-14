// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The compact skill index of deferred skill disclosure.
//!
//! Each skill is one line, its command and a summary of at most
//! [`SUMMARY_CHARS`](crate::instructions::SUMMARY_CHARS) characters. Above
//! [`INDEX_LINE_LIMIT`] skills the index gives the number of skills in each
//! package instead, so a project with hundreds of skills does not pay for
//! hundreds of lines on every request. A pinned skill (`sven.always`) keeps
//! its full entry whatever the mode, and a skill the user may only invoke is
//! left out of the model's listing, as in the full listing.

use std::collections::BTreeMap;

use sven_workspace::SkillInfo;

use crate::instructions::{summary, SUMMARY_CHARS};

/// The most skills the index names one by one.
pub const INDEX_LINE_LIMIT: usize = 50;

/// Phrases that open many descriptions and say nothing about the skill.
const BOILERPLATE: &[&str] = &[
    "this skill should be used when ",
    "use this skill when ",
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
