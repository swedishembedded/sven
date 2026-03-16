// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Skill-based slash commands loaded from SKILL.md files.

use std::fs;
use std::path::PathBuf;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use sven_runtime::{AgentInfo, SkillInfo};

use crate::commands::{CommandContext, CommandResult, CompletionItem, SlashCommand};

const MAX_DESCRIPTION_LEN: usize = 100;

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn truncate_to_width_exact(s: &str, max: usize) -> String {
    let mut width = 0;
    let mut end = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > max {
            break;
        }
        width += w;
        end += c.len_utf8();
    }
    s[..end].to_string()
}

fn truncate_description(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if display_width(trimmed) <= max {
        trimmed.to_string()
    } else {
        format!(
            "{}…",
            truncate_to_width_exact(trimmed, max.saturating_sub(1))
        )
    }
}

// ── SkillCommand ──────────────────────────────────────────────────────────────

/// A slash command backed by a command `.md` file.
pub struct SkillCommand {
    pub name: String,
    pub description: String,
    pub source_path: PathBuf,
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

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let content = match fs::read_to_string(&self.source_path) {
            Ok(s) => s,
            Err(e) => {
                return CommandResult {
                    message_to_send: Some(format!(
                        "Error: Failed to read command file {}: {e}",
                        self.source_path.display()
                    )),
                    ..Default::default()
                };
            }
        };
        CommandResult {
            message_to_send: Some(build_content_message(&content, &args)),
            ..Default::default()
        }
    }
}

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

#[must_use]
pub fn make_command_slash_commands(commands: &[SkillInfo]) -> Vec<SkillCommand> {
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(commands.len());

    for cmd in commands {
        let normalised = cmd
            .command
            .split('/')
            .map(|seg| seg.to_lowercase())
            .collect::<Vec<_>>()
            .join("/");

        let unique_name = resolve_unique_command_name(normalised, &mut used_names);

        result.push(SkillCommand {
            name: unique_name,
            description: truncate_description(&cmd.description, MAX_DESCRIPTION_LEN),
            source_path: cmd.skill_md_path.clone(),
            skill_dir: cmd.skill_dir.clone(),
        });
    }

    result
}

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
pub struct AgentCommand {
    pub name: String,
    pub description: String,
    pub content: String,
    pub model_override: Option<String>,
}

impl SlashCommand for AgentCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandContext;
    use std::path::PathBuf;
    use std::sync::Arc;
    use sven_config::Config;
    use sven_runtime::AgentInfo;

    fn make_cmd(name: &str, description: &str, content: &str) -> (SkillCommand, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cmd.md");
        std::fs::write(&path, content).unwrap();
        let cmd = SkillCommand {
            name: name.to_string(),
            description: truncate_description(description, MAX_DESCRIPTION_LEN),
            source_path: path.to_path_buf(),
            skill_dir: dir.path().to_path_buf(),
        };
        (cmd, dir)
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
            knowledge: vec![],
        }
    }

    #[test]
    fn execute_no_args_returns_content() {
        let (cmd, _guard) = make_cmd("sven/plan", "Plan.", "## Instructions\n\nDo things.");
        let result = cmd.execute(vec![]);
        assert_eq!(
            result.message_to_send.as_deref(),
            Some("## Instructions\n\nDo things.")
        );
    }

    #[test]
    fn execute_with_task_appends_task() {
        let (cmd, _guard) = make_cmd("sven/plan", "Plan.", "## Instructions\n\nDo things.");
        let result = cmd.execute(vec!["fix the CI".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("## Instructions"));
        assert!(msg.contains("Task: fix the CI"));
    }

    #[test]
    fn complete_returns_empty() {
        let (cmd, _guard) = make_cmd("test", "Test.", "body");
        assert!(cmd.complete(0, "partial", &ctx()).is_empty());
    }

    #[test]
    fn command_hyphens_preserved() {
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
    fn agent_command_injects_prompt() {
        let agent = make_agent("verifier", "Validates work.", "You verify.", None);
        let cmds = make_agent_slash_commands(&[agent]);
        let result = cmds[0].execute(vec!["check auth".to_string()]);
        let msg = result.message_to_send.unwrap();
        assert!(msg.contains("You verify."));
        assert!(msg.contains("Task: check auth"));
    }

    #[test]
    fn agent_name_lowercased() {
        let agent = make_agent("Security-Auditor", "Security.", "body", None);
        let cmds = make_agent_slash_commands(&[agent]);
        assert_eq!(cmds[0].name, "security-auditor");
    }
}
