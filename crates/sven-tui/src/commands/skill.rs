// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
#![allow(dead_code)]
//! Skill-based slash commands loaded from `.sven/skills/<name>/SKILL.md`.
//!
//! **This module is a stub.**  The discovery function currently returns an
//! empty list.  It will be implemented when Skills support is added.
//!
//! Planned behaviour:
//! - Scan `skills_dir` for `<skill-name>/SKILL.md` files
//! - Parse the SKILL.md for metadata (command name, arguments, description)
//! - Register each skill as a slash command that injects the skill content
//!   into the user's message

use std::path::Path;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

/// A slash command backed by a `.sven/skills/<name>/SKILL.md` file.
pub struct SkillCommand {
    pub name: String,
    pub path: std::path::PathBuf,
    // Future fields: description, arguments, template body
}

impl SlashCommand for SkillCommand {
    fn name(&self) -> &str { &self.name }

    fn description(&self) -> &str { "Skill command (from SKILL.md)" }

    fn arguments(&self) -> Vec<CommandArgument> {
        // Future: parse argument declarations from SKILL.md frontmatter
        vec![]
    }

    fn complete(&self, _arg_index: usize, _partial: &str, _ctx: &CommandContext) -> Vec<CompletionItem> {
        // Future: extract argument completion hints from SKILL.md
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        // Future: render SKILL.md template with args and return as message_to_send
        CommandResult::default()
    }
}

/// Scan `skills_dir` for skill commands.
///
/// **Currently returns an empty vec** (stub implementation).
pub async fn discover_skills(_skills_dir: &Path) -> Vec<SkillCommand> {
    // TODO: implement when Skills support is added
    vec![]
}
