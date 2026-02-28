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

use sven_runtime::{AgentInfo, SkillInfo};

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

const MAX_DESCRIPTION_LEN: usize = 100;

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
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::optional(
            "task",
            "Optional task to perform using this skill",
        )]
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
        CommandResult {
            message_to_send: Some(build_content_message(&self.content, &args)),
            ..Default::default()
        }
    }
}

/// Build the message to send when a content-based slash command is executed.
///
/// When `args` is empty the content is sent as-is.  When a task is provided
/// it is appended with a `\n\nTask:` separator so the model can distinguish
/// the injected instructions from the user's intent.
fn build_content_message(content: &str, args: &[String]) -> String {
    let task = args.join(" ");
    let task = task.trim();
    if task.is_empty() {
        content.trim_end().to_string()
    } else {
        format!("{}\n\nTask: {task}", content.trim_end())
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

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
        let normalised = cmd
            .command
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

/// Append a numeric suffix separated by `sep` until the name is unique.
///
/// Used by both skill commands (`sep = '_'`) and user commands / agents
/// (`sep = '-'`) so that the disambiguation suffix matches the filename
/// convention of each kind.
fn resolve_unique_name_sep(
    base: String,
    sep: char,
    used: &mut std::collections::HashSet<String>,
) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for i in 2..1000usize {
        let candidate = format!("{base}{sep}{i}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    let fallback = format!("{base}{sep}x");
    used.insert(fallback.clone());
    fallback
}

fn resolve_unique_command_name(
    base: String,
    used: &mut std::collections::HashSet<String>,
) -> String {
    resolve_unique_name_sep(base, '-', used)
}

// ── AgentCommand ──────────────────────────────────────────────────────────────

/// A slash command backed by a subagent markdown file.
///
/// When invoked, injects the subagent's system prompt (body) as a message
/// prefix, optionally followed by the user-provided task.  The `model_override`
/// field carries the `model:` frontmatter value so the app can switch models
/// for the turn.
pub struct AgentCommand {
    /// Slash command name derived from the agent's `name` field (lowercase).
    pub name: String,
    /// One-line description from frontmatter.
    pub description: String,
    /// Agent system prompt body, injected into the message.
    pub content: String,
    /// Optional model override from frontmatter (`None` → use session model).
    pub model_override: Option<String>,
}

impl SlashCommand for AgentCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::optional(
            "task",
            "Optional task for this subagent",
        )]
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
        CommandResult {
            message_to_send: Some(build_content_message(&self.content, &args)),
            model_override: self.model_override.clone(),
            ..Default::default()
        }
    }
}

/// Build [`AgentCommand`] instances from a slice of discovered subagents.
///
/// Each agent becomes a slash command keyed by the lowercased agent name with
/// hyphens preserved (e.g. `security-auditor` → `/security-auditor`).
/// Duplicate names are disambiguated by appending `-2`, `-3`, etc.
#[must_use]
pub fn make_agent_slash_commands(agents: &[AgentInfo]) -> Vec<AgentCommand> {
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(agents.len());

    for agent in agents {
        let base = agent.name.to_lowercase();
        let unique_name = resolve_unique_command_name(base, &mut used);

        result.push(AgentCommand {
            name: unique_name,
            description: truncate_description(&agent.description, MAX_DESCRIPTION_LEN),
            content: agent.content.clone(),
            model_override: agent.model.clone(),
        });
    }

    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use std::path::PathBuf;
    use std::sync::Arc;
    use sven_config::Config;
    use sven_runtime::AgentInfo;

    fn make_cmd(name: &str, description: &str, content: &str) -> SkillCommand {
        SkillCommand {
            name: name.to_string(),
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

    fn make_agent(name: &str, description: &str, content: &str, model: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: name.to_string(),
            description: description.to_string(),
            model: model.map(|s| s.to_string()),
            readonly: false,
            is_background: false,
            content: content.to_string(),
            agent_md_path: PathBuf::from(format!("/tmp/agents/{name}.md")),
        }
    }

    // ── SkillCommand::execute ─────────────────────────────────────────────────

    #[test]
    fn execute_no_args_returns_content() {
        let cmd = make_cmd("sven/plan", "Plan.", "## Instructions\n\nDo things.");
        let result = cmd.execute(vec![]);
        assert_eq!(
            result.message_to_send.as_deref(),
            Some("## Instructions\n\nDo things.")
        );
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
        let result = cmd.execute(vec![
            "push".to_string(),
            "to".to_string(),
            "prod".to_string(),
        ]);
        assert!(result
            .message_to_send
            .unwrap()
            .contains("Task: push to prod"));
    }

    #[test]
    fn execute_whitespace_only_args_treated_as_no_task() {
        let cmd = make_cmd("deploy", "Deploy.", "Deploy content.");
        let result = cmd.execute(vec!["  ".to_string()]);
        assert_eq!(result.message_to_send.as_deref(), Some("Deploy content."));
    }

    #[test]
    fn complete_returns_empty() {
        let cmd = make_cmd("test", "Test.", "body");
        assert!(cmd.complete(0, "partial", &ctx()).is_empty());
    }

    #[test]
    fn description_truncated_to_100_chars() {
        let long_desc = "x".repeat(120);
        let cmd = make_cmd("cmd", &long_desc, "body");
        assert!(cmd.description.chars().count() <= MAX_DESCRIPTION_LEN);
    }

    // ── make_command_slash_commands ───────────────────────────────────────────

    #[test]
    fn command_hyphens_preserved() {
        use sven_runtime::SkillInfo;
        let info = SkillInfo {
            command: "review-code".to_string(),
            name: "review-code".to_string(),
            description: "Review code.".to_string(),
            version: None,
            skill_md_path: PathBuf::from("/tmp/review-code.md"),
            skill_dir: PathBuf::from("/tmp"),
            content: "body".to_string(),
            sven_meta: None,
        };
        let cmds = make_command_slash_commands(&[info]);
        assert_eq!(cmds[0].name, "review-code");
    }

    #[test]
    fn command_duplicates_get_hyphen_suffix() {
        use sven_runtime::SkillInfo;
        let make = |cmd: &str| SkillInfo {
            command: cmd.to_string(),
            name: cmd.to_string(),
            description: "D.".to_string(),
            version: None,
            skill_md_path: PathBuf::from("/tmp/a.md"),
            skill_dir: PathBuf::from("/tmp"),
            content: "b".to_string(),
            sven_meta: None,
        };
        let cmds = make_command_slash_commands(&[make("deploy"), make("deploy")]);
        assert_eq!(cmds.len(), 2);
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"deploy"));
        assert!(names.contains(&"deploy-2"));
    }

    // ── AgentCommand ─────────────────────────────────────────────────────────

    #[test]
    fn agent_command_injects_prompt() {
        let agent = make_agent("verifier", "Validates work.", "You verify.", None);
        let cmds = make_agent_slash_commands(&[agent]);
        let result = cmds[0].execute(vec!["check auth".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("You verify."));
        assert!(msg.contains("Task: check auth"));
    }

    #[test]
    fn agent_command_carries_model_override() {
        let agent = make_agent("fast-agent", "Fast.", "body", Some("fast"));
        let cmds = make_agent_slash_commands(&[agent]);
        assert_eq!(cmds[0].model_override.as_deref(), Some("fast"));
    }

    #[test]
    fn agent_command_no_model_when_inherit() {
        let agent = AgentInfo {
            name: "agent".to_string(),
            description: "D.".to_string(),
            model: None, // already normalised by parse_agent_file
            readonly: false,
            is_background: false,
            content: "body".to_string(),
            agent_md_path: PathBuf::from("/tmp/agent.md"),
        };
        let cmds = make_agent_slash_commands(&[agent]);
        assert!(cmds[0].model_override.is_none());
    }

    #[test]
    fn agent_name_lowercased() {
        let agent = make_agent("Security-Auditor", "Security.", "body", None);
        let cmds = make_agent_slash_commands(&[agent]);
        assert_eq!(cmds[0].name, "security-auditor");
    }

    // ── resolve_unique_name_sep ───────────────────────────────────────────────

    #[test]
    fn resolve_unique_underscore_sep() {
        let mut used = std::collections::HashSet::new();
        assert_eq!(
            resolve_unique_name_sep("foo".to_string(), '_', &mut used),
            "foo"
        );
        assert_eq!(
            resolve_unique_name_sep("foo".to_string(), '_', &mut used),
            "foo_2"
        );
        assert_eq!(
            resolve_unique_name_sep("foo".to_string(), '_', &mut used),
            "foo_3"
        );
    }

    #[test]
    fn resolve_unique_hyphen_sep() {
        let mut used = std::collections::HashSet::new();
        assert_eq!(
            resolve_unique_name_sep("bar".to_string(), '-', &mut used),
            "bar"
        );
        assert_eq!(
            resolve_unique_name_sep("bar".to_string(), '-', &mut used),
            "bar-2"
        );
    }
}
