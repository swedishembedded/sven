// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Ranked search over the skills a session can load.
//!
//! The skills are ranked exactly as tools are: the same [`ToolRanker`], BM25 by
//! default, so a decision model re-ranking tool search can re-rank this too.
//! A skill is described to the ranker by its command and its whole description,
//! which is where the trigger phrases that say when to use it live.

use sven_hsm::ToolCapability;
use sven_tool_registry::{ToolDescriptor, ToolRanker};
use sven_workspace::SkillInfo;

/// The most skills one search returns.
pub(crate) const MAX_RESULTS: usize = 8;

/// The longest summary of a result, in characters.
const SUMMARY_CHARS: usize = 160;

/// The skills the model may discover: those the user does not reserve for
/// explicit invocation.
pub(crate) fn discoverable(skills: &[SkillInfo]) -> Vec<&SkillInfo> {
    skills
        .iter()
        .filter(|s| !s.sven_meta.as_ref().is_some_and(|m| m.user_invocable_only))
        .collect()
}

fn descriptor(skill: &SkillInfo) -> ToolDescriptor {
    ToolDescriptor {
        name: skill.command.clone(),
        summary: skill
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        capability: ToolCapability::ReadFile,
        mcp: false,
        description: String::new(),
        parameter_descriptions: Vec::new(),
    }
}

fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SUMMARY_CHARS {
        return line;
    }
    let cut: String = line.chars().take(SUMMARY_CHARS - 3).collect();
    format!("{}...", cut.trim_end())
}

/// The best [`MAX_RESULTS`] skills for `query`, as the tool's result text.
pub(crate) async fn search(ranker: &dyn ToolRanker, skills: &[SkillInfo], query: &str) -> String {
    let candidates = discoverable(skills);
    let descriptors: Vec<ToolDescriptor> = candidates.iter().map(|s| descriptor(s)).collect();
    let ranked = ranker.rank(query, &descriptors).await;
    let lines: Vec<String> = ranked
        .iter()
        .filter_map(|r| candidates.iter().find(|s| s.command == r.name))
        .take(MAX_RESULTS)
        .map(|s| {
            format!(
                "  <skill command=\"{}\" name=\"{}\">{}</skill>",
                s.command,
                s.name,
                shorten(&s.description)
            )
        })
        .collect();
    if lines.is_empty() {
        return format!(
            "(no skill matched \"{query}\"; {} skills exist, `action: \"list\"` shows them)",
            candidates.len()
        );
    }
    format!(
        "<skills count=\"{}\" of=\"{}\">\n{}\n</skills>",
        lines.len(),
        candidates.len(),
        lines.join("\n")
    )
}
