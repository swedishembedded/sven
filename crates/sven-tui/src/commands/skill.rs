// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Skill-based slash commands loaded from SKILL.md files.
//!
//! Each discovered skill is registered as a slash command whose name is the
//! sanitized skill command path.  Hierarchical skills use `/` as a separator
//! so that `sven/plan` becomes the command `/sven/plan` in the TUI.
//!
//! When invoked, the command injects the skill's full SKILL.md body —
//! optionally followed by a user-provided task — as the message to send to
//! the agent.  Sub-skill bodies are never pre-loaded; the parent body
//! instructs the model to call `load_skill(<command>)` for each step.
//!
//! ## Usage
//!
//! ```text
//! /sven                    → send parent skill content alone
//! /sven/plan               → send sven/plan skill content alone
//! /sven/plan analyse task  → send sven/plan content + "Task: analyse task"
//! ```

use std::path::PathBuf;

use sven_runtime::SkillInfo;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

// ── Name sanitization ──────────────────────────────────────────────────────────

const MAX_COMMAND_NAME_LEN: usize = 64; // longer limit to accommodate path segments
const MAX_DESCRIPTION_LEN: usize = 100;

/// Sanitize a single path segment into a valid slash command keyword component.
///
/// - Converts to lowercase
/// - Replaces any sequence of non-alphanumeric characters with `_`
/// - Strips leading and trailing `_`
/// - Truncates to `max_len` characters
///
/// Returns `"skill"` if the result would otherwise be empty.
fn sanitize_segment(raw: &str, max_len: usize) -> String {
    let lower = raw.to_lowercase();
    let mut result = String::with_capacity(lower.len());
    let mut in_sep = false;

    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
            in_sep = false;
        } else if !in_sep && !result.is_empty() {
            result.push('_');
            in_sep = true;
        }
    }

    let trimmed = result.trim_end_matches('_');
    let truncated: String = trimmed.chars().take(max_len).collect();

    if truncated.is_empty() {
        "skill".to_string()
    } else {
        truncated
    }
}

/// Sanitize a skill command path into a valid slash command keyword.
///
/// The `/` path separator is preserved; each segment is individually
/// sanitized via [`sanitize_segment`].
///
/// ```text
/// "sven"          → "sven"
/// "sven/plan"     → "sven/plan"
/// "My Skill"      → "my_skill"
/// "git-workflow"  → "git_workflow"
/// ```
#[must_use]
pub fn sanitize_command_name(raw: &str) -> String {
    // Per-segment max is generous; the overall limit exists to bound total length.
    let per_segment_max = MAX_COMMAND_NAME_LEN;
    let sanitized = raw
        .split('/')
        .map(|seg| sanitize_segment(seg, per_segment_max))
        .collect::<Vec<_>>()
        .join("/");

    // Overall length guard: truncate at a segment boundary.
    if sanitized.len() <= MAX_COMMAND_NAME_LEN {
        return sanitized;
    }
    let mut len = 0usize;
    sanitized
        .split('/')
        .take_while(|seg| {
            let next = len + seg.len() + if len == 0 { 0 } else { 1 };
            if next <= MAX_COMMAND_NAME_LEN {
                len = next;
                true
            } else {
                false
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn truncate_description(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let mut result: String = trimmed.chars().take(max.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

// ── SkillCommand ──────────────────────────────────────────────────────────────

/// A slash command backed by a `SKILL.md` file.
///
/// Injecting the skill content into the conversation is intentionally cheap:
/// only the parent skill's SKILL.md body is sent.  The body instructs the
/// model to call `load_skill(<command>)` for each step when needed — sub-skill
/// bodies are never pre-loaded.  This keeps token usage proportional to what
/// the current task actually requires.
pub struct SkillCommand {
    /// Sanitized command path (e.g. `"sven_plan"` for a skill at `"sven/plan"`
    /// — but since we preserve `/`, this is `"sven/plan"`).
    pub name: String,
    /// One-line description (truncated to 100 chars).
    pub description: String,
    /// Full SKILL.md body, cached at discovery time.
    pub content: String,
    /// Absolute path to the skill directory (for future script resolution).
    #[allow(dead_code)]
    pub skill_dir: PathBuf,
}

impl SlashCommand for SkillCommand {
    fn name(&self) -> &str { &self.name }

    fn description(&self) -> &str { &self.description }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::optional("task", "Optional task to perform using this skill")]
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let task = args.join(" ");
        let task = task.trim();
        let message = if task.is_empty() {
            self.content.trim_end().to_string()
        } else {
            format!("{}\n\nTask: {task}", self.content.trim_end())
        };
        CommandResult { message_to_send: Some(message), ..Default::default() }
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Build [`SkillCommand`] instances from a slice of discovered skills.
///
/// Each skill becomes a slash command keyed by its sanitized command path.
/// Skills with `user_invocable_only: true` are included (slash commands are
/// explicitly user-invoked).  Duplicate sanitized names are resolved by
/// appending `_2`, `_3`, etc. to the last segment only.
///
/// Only the skill's own SKILL.md body is stored — sub-skill bodies are never
/// pre-loaded.
#[must_use]
pub fn make_skill_commands(skills: &[SkillInfo]) -> Vec<SkillCommand> {
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut commands = Vec::with_capacity(skills.len());

    for skill in skills {
        let base = sanitize_command_name(&skill.command);
        let unique_name = resolve_unique_name(base, &mut used_names);

        commands.push(SkillCommand {
            name: unique_name,
            description: truncate_description(&skill.description, MAX_DESCRIPTION_LEN),
            content: skill.content.clone(),
            skill_dir: skill.skill_dir.clone(),
        });
    }

    commands
}

/// Build [`SkillCommand`] instances from a slice of user-authored commands.
///
/// Unlike [`make_skill_commands`], this function preserves the command name
/// exactly as derived from the filename (e.g. `review-code.md` → `/review-code`).
/// Only the last-path-component is lowercased; hyphens are kept intact so
/// commands behave identically to Cursor's `.cursor/commands/` convention.
///
/// Duplicate names are disambiguated by appending `-2`, `-3`, etc.
#[must_use]
pub fn make_command_slash_commands(commands: &[SkillInfo]) -> Vec<SkillCommand> {
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(commands.len());

    for cmd in commands {
        // Normalise: lowercase each path segment, preserve hyphens and slashes.
        let normalised = cmd.command
            .split('/')
            .map(|seg| seg.to_lowercase())
            .collect::<Vec<_>>()
            .join("/");

        // Deduplicate with hyphen-style suffix to stay consistent with the
        // filename convention (e.g. `review-code`, `review-code-2`).
        let unique_name = resolve_unique_command_name(normalised, &mut used_names);

        result.push(SkillCommand {
            name: unique_name,
            description: truncate_description(&cmd.description, MAX_DESCRIPTION_LEN),
            content: cmd.content.clone(),
            skill_dir: cmd.skill_dir.clone(),
        });
    }

    result
}

fn resolve_unique_command_name(
    base: String,
    used: &mut std::collections::HashSet<String>,
) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for i in 2..1000usize {
        let candidate = format!("{base}-{i}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    let fallback = format!("{base}-x");
    used.insert(fallback.clone());
    fallback
}

fn resolve_unique_name(base: String, used: &mut std::collections::HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for i in 2..1000usize {
        let candidate = format!("{base}_{i}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    // Fallback: should never be reached in practice
    let fallback = format!("{base}_x");
    used.insert(fallback.clone());
    fallback
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use sven_config::Config;
    use sven_runtime::{SkillInfo, SvenSkillMeta};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn make_skill(command: &str, description: &str, content: &str) -> SkillInfo {
        let name = command.rsplit('/').next().unwrap_or(command).to_string();
        SkillInfo {
            command: command.to_string(),
            name,
            description: description.to_string(),
            version: None,
            skill_md_path: PathBuf::from(format!("/tmp/skills/{command}/SKILL.md")),
            skill_dir: PathBuf::from(format!("/tmp/skills/{command}")),
            content: content.to_string(),
            sven_meta: None,
        }
    }

    fn make_cmd(command: &str, description: &str, content: &str) -> SkillCommand {
        SkillCommand {
            name: sanitize_command_name(command),
            description: truncate_description(description, MAX_DESCRIPTION_LEN),
            content: content.to_string(),
            skill_dir: PathBuf::from("/tmp"),
        }
    }

    fn ctx() -> CommandContext {
        CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: String::new(),
            current_model_name: String::new(),
        }
    }

    // ── sanitize_command_name ─────────────────────────────────────────────────

    #[test]
    fn sanitize_kebab_becomes_underscore() {
        assert_eq!(sanitize_command_name("git-workflow"), "git_workflow");
    }

    #[test]
    fn sanitize_slash_preserved() {
        assert_eq!(sanitize_command_name("sven/plan"), "sven/plan");
    }

    #[test]
    fn sanitize_nested_path_segments_cleaned() {
        assert_eq!(sanitize_command_name("my-skill/sub-step"), "my_skill/sub_step");
    }

    #[test]
    fn sanitize_already_clean() {
        assert_eq!(sanitize_command_name("deploy"), "deploy");
    }

    #[test]
    fn sanitize_spaces_become_underscore() {
        assert_eq!(sanitize_command_name("my skill"), "my_skill");
    }

    #[test]
    fn sanitize_multiple_separators_collapse() {
        assert_eq!(sanitize_command_name("my--skill"), "my_skill");
    }

    #[test]
    fn sanitize_empty_falls_back_to_skill() {
        assert_eq!(sanitize_command_name(""), "skill");
        assert_eq!(sanitize_command_name("---"), "skill");
    }

    #[test]
    fn sanitize_uppercase_lowercased() {
        assert_eq!(sanitize_command_name("GitWorkflow"), "gitworkflow");
    }

    #[test]
    fn sanitize_deep_path_preserved() {
        assert_eq!(sanitize_command_name("sven/implement/research"), "sven/implement/research");
    }

    // ── SkillCommand::execute ─────────────────────────────────────────────────

    #[test]
    fn execute_no_args_returns_content() {
        let cmd = make_cmd("sven/plan", "Plan.", "## Instructions\n\nDo things.");
        let result = cmd.execute(vec![]);
        assert_eq!(result.message_to_send.as_deref(), Some("## Instructions\n\nDo things."));
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.immediate_action.is_none());
    }

    #[test]
    fn execute_with_task_appends_task() {
        let cmd = make_cmd("sven/plan", "Plan.", "## Instructions\n\nDo things.");
        let result = cmd.execute(vec!["fix the CI".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("## Instructions"));
        assert!(msg.contains("Task: fix the CI"));
    }

    #[test]
    fn execute_multi_word_task_joined() {
        let cmd = make_cmd("deploy", "Deploy.", "Deploy content.");
        let result = cmd.execute(vec!["push".to_string(), "to".to_string(), "prod".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("Task: push to prod"));
    }

    #[test]
    fn execute_whitespace_only_args_treated_as_no_task() {
        let cmd = make_cmd("deploy", "Deploy.", "Deploy content.");
        let result = cmd.execute(vec!["  ".to_string()]);
        assert_eq!(result.message_to_send.as_deref(), Some("Deploy content."));
    }

    // ── make_skill_commands ───────────────────────────────────────────────────

    #[test]
    fn make_skill_commands_uses_command_path() {
        let skills = vec![make_skill("sven/plan", "Plan.", "body")];
        let cmds = make_skill_commands(&skills);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "sven/plan");
    }

    #[test]
    fn make_skill_commands_top_level_skill() {
        let skills = vec![make_skill("git-workflow", "Git helper.", "body")];
        let cmds = make_skill_commands(&skills);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "git_workflow");
    }

    #[test]
    fn make_skill_commands_duplicate_names_deduplicated() {
        let skills = vec![
            make_skill("my-skill", "First.", "body1"),
            make_skill("my_skill", "Second.", "body2"),
        ];
        let cmds = make_skill_commands(&skills);
        assert_eq!(cmds.len(), 2);
        assert_ne!(cmds[0].name, cmds[1].name);
        let names: std::collections::HashSet<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains("my_skill"));
        assert!(names.contains("my_skill_2"));
    }

    #[test]
    fn make_skill_commands_includes_user_invocable_only_skills() {
        let mut skill = make_skill("private-skill", "User-only.", "Private body.");
        skill.sven_meta = Some(SvenSkillMeta { user_invocable_only: true, ..Default::default() });
        let cmds = make_skill_commands(&[skill]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "private_skill");
    }

    #[test]
    fn description_truncated_to_100_chars() {
        let long_desc = "x".repeat(120);
        let skill = make_skill("long-desc", &long_desc, "body");
        let cmds = make_skill_commands(&[skill]);
        assert!(cmds[0].description.chars().count() <= MAX_DESCRIPTION_LEN);
    }

    #[test]
    fn complete_returns_empty() {
        let cmd = make_cmd("test", "Test.", "body");
        let completions = cmd.complete(0, "partial", &ctx());
        assert!(completions.is_empty());
    }

    // ── Hierarchical skill tests ──────────────────────────────────────────────

    #[test]
    fn parent_and_subskill_registered_as_independent_commands() {
        let parent = make_skill("sven", "Top-level.", "## Sven\n\nCall load_skill('sven/plan').");
        let child  = make_skill("sven/plan", "Planning.", "## Planning detail.");
        let cmds = make_skill_commands(&[parent, child]);
        assert!(cmds.iter().any(|c| c.name == "sven"), "parent registered");
        assert!(cmds.iter().any(|c| c.name == "sven/plan"), "child registered independently");
    }

    #[test]
    fn slash_command_sends_only_own_body() {
        let parent = make_skill(
            "sven",
            "Top-level sven workflow.",
            "## Sven Workflow\n\nFor planning: call load_skill('sven/plan').",
        );
        let child = make_skill(
            "sven/plan",
            "Planning sub-skill.",
            "## Planning\n\nThis should NOT appear in the parent slash command output.",
        );
        let cmds = make_skill_commands(&[parent, child]);
        let sven_cmd = cmds.iter().find(|c| c.name == "sven").unwrap();

        let result = sven_cmd.execute(vec![]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("Sven Workflow"), "parent content present");
        assert!(!msg.contains("This should NOT appear"), "child body must not be injected");
    }

    #[test]
    fn child_slash_command_sends_only_child_body() {
        let parent = make_skill("sven", "Top-level.", "Parent body.");
        let child  = make_skill("sven/plan", "Plan.", "Child body.");
        let cmds = make_skill_commands(&[parent, child]);
        let plan_cmd = cmds.iter().find(|c| c.name == "sven/plan").unwrap();

        let result = plan_cmd.execute(vec![]);
        let msg = result.message_to_send.unwrap();
        assert_eq!(msg, "Child body.");
    }

    #[test]
    fn slash_command_with_task_appends_only_own_body_plus_task() {
        let parent = make_skill(
            "sven",
            "Sven workflow.",
            "## Workflow\n\nCall load_skill('sven/plan') to plan.",
        );
        let child = make_skill("sven/plan", "Plan.", "Sub-skill body — must not appear.");
        let cmds = make_skill_commands(&[parent, child]);
        let sven_cmd = cmds.iter().find(|c| c.name == "sven").unwrap();

        let result = sven_cmd.execute(vec!["implement feature X".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("## Workflow"), "parent content present");
        assert!(msg.contains("Task: implement feature X"), "task appended");
        assert!(!msg.contains("Sub-skill body"), "child body must not be injected");
    }

    #[test]
    fn standalone_skill_body_sent_without_extra_blocks() {
        let skill = make_skill("standalone", "Standalone skill.", "Just a body.");
        let cmds = make_skill_commands(&[skill]);
        let result = cmds[0].execute(vec![]);
        let msg = result.message_to_send.unwrap();
        assert_eq!(msg, "Just a body.");
    }
}
