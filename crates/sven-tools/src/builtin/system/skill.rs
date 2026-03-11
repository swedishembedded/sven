// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compound `skill` tool that loads and lists agent skills on demand.
//!
//! Actions:
//! - `load` — load a named skill's full SKILL.md content into conversation context.
//! - `list` — list available skills, optionally filtered by a regex.
//!
//! The model calls `load` after recognising that a user request matches one
//! of the skills shown by `list` or listed in the system prompt's
//! `<available_skills>` block.  The `load` action returns:
//!
//! - The full SKILL.md body (everything after the frontmatter fence).
//! - The absolute path to the skill directory so the model can resolve bundled
//!   resources (`scripts/`, `references/`, `assets/`) relative to it.
//! - A sampled listing of up to [`MAX_BUNDLED_FILES`] bundled file paths so
//!   the model knows what resources are available without reading them all.
//! - A compact navigation hint listing **direct child sub-skills** (name +
//!   one-line description) when the skill has nested skill packages below it.

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use tracing::debug;

use sven_runtime::{SharedSkills, SkillInfo};

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Maximum number of bundled file paths to list in the tool response.
const MAX_BUNDLED_FILES: usize = 20;

/// Build the static description string for the tool, listing available skills.
fn build_description(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return "Load or list agent skills.\n\
                action: load | list\n\n\
                No skills are currently available."
            .to_string();
    }

    let skill_list: String = skills
        .iter()
        .filter(|s| !s.sven_meta.as_ref().is_some_and(|m| m.user_invocable_only))
        .map(|s| {
            format!(
                "  <skill>\n    <command>{}</command>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
                s.command,
                s.name,
                s.description.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Load or list agent skills.\n\
         action: load | list\n\n\
         load: Load the full instructions for a named skill into the conversation context.\n\
         Call this when the user's request matches a skill description.\n\
         Returns the skill's SKILL.md body, the absolute base directory, \
         and a listing of bundled files.\n\
         Pass the <command> value (e.g. \"sven\" or \"sven/plan\") as the `name` argument.\n\n\
         list: List available skills, optionally filtered by a regex against command/name/description.\n\
         Leave `regex` empty to list all skills.\n\n\
         <available_skills>\n{skill_list}\n</available_skills>"
    )
}

/// Return the direct children of `parent` in the skill hierarchy.
fn direct_children<'a>(parent: &SkillInfo, all: &'a [SkillInfo]) -> Vec<&'a SkillInfo> {
    let prefix = format!("{}/", parent.command);
    all.iter()
        .filter(|s| s.command.starts_with(&prefix) && !s.command[prefix.len()..].contains('/'))
        .collect()
}

/// Build a compact `<sub_skills>` navigation block listing direct child skills.
fn build_sub_skills_hint(parent: &SkillInfo, all: &[SkillInfo]) -> String {
    let children = direct_children(parent, all);
    if children.is_empty() {
        return String::new();
    }

    let lines: Vec<String> = children
        .iter()
        .map(|child| {
            let one_liner = child.description.lines().next().unwrap_or("").trim();
            format!(
                "  <sub_skill command=\"{}\" name=\"{}\">{}</sub_skill>",
                child.command, child.name, one_liner
            )
        })
        .collect();

    format!(
        "\n\n<sub_skills>\n\
         <!-- Call skill(action=load, name=<command>) to load any sub-skill's full instructions. -->\n\
         {}\n\
         </sub_skills>",
        lines.join("\n")
    )
}

/// Compound skill tool — load and list agent skills.
pub struct SkillTool {
    /// Live-refreshable skill collection shared with the TUI.
    skills: SharedSkills,
    /// Pre-computed description string (may be slightly stale after a refresh;
    /// the system prompt's `<available_skills>` block is always current).
    description: String,
}

impl SkillTool {
    /// Create a new `SkillTool` from a [`SharedSkills`] instance.
    pub fn new(skills: SharedSkills) -> Self {
        let description = build_description(&skills.get());
        Self {
            skills,
            description,
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["load", "list"],
                    "description": "Which skill action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "[action=load] The command of the skill to load \
                                    (e.g. \"sven\" or \"sven/plan\")"
                },
                "regex": {
                    "type": "string",
                    "description": "[action=list] Optional regex to filter skills by command, \
                                    name, or description. Leave empty to list all skills."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        match action.as_str() {
            "load" => self.exec_load(call).await,
            "list" => self.exec_list(call).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action '{other}'. Valid: load, list"),
            ),
        }
    }
}

impl SkillTool {
    async fn exec_load(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'name' parameter for action=load"),
        };

        debug!(skill = %command, "skill tool load");

        let current_skills = self.skills.get();

        let skill = match current_skills.iter().find(|s| s.command == command) {
            Some(s) => s,
            None => {
                let available = current_skills
                    .iter()
                    .map(|s| s.command.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "skill \"{command}\" not found. Available skills: {}",
                        if available.is_empty() {
                            "(none)"
                        } else {
                            &available
                        }
                    ),
                );
            }
        };

        let mut bundled_files: Vec<String> = Vec::new();
        collect_files_recursive(
            &skill.skill_dir,
            &mut bundled_files,
            &skill.skill_md_path,
            true,
        );
        bundled_files.sort();
        bundled_files.truncate(MAX_BUNDLED_FILES);

        let files_block = if bundled_files.is_empty() {
            String::new()
        } else {
            let list = bundled_files
                .iter()
                .map(|p| format!("<file>{p}</file>"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\n\n<skill_files>\n{list}\n</skill_files>")
        };

        let base_dir = skill.skill_dir.display().to_string();
        let content = skill.content.trim_end();
        let sub_skills_hint = build_sub_skills_hint(skill, &current_skills);

        ToolOutput::ok(
            &call.id,
            format!(
                "<skill_content command=\"{command}\" name=\"{name}\">\n\
                 # Skill: {name}\n\n\
                 {content}\n\n\
                 Base directory: {base_dir}\n\
                 Relative paths in this skill (scripts/, references/, assets/) \
                 are relative to this base directory.\
                 {files_block}\
                 {sub_skills_hint}\n\
                 </skill_content>",
                name = skill.name
            ),
        )
    }

    async fn exec_list(&self, call: &ToolCall) -> ToolOutput {
        let regex_str = call
            .args
            .get("regex")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        debug!(regex = %regex_str, "skill tool list");

        let compiled_re = if regex_str.is_empty() {
            None
        } else {
            match Regex::new(regex_str) {
                Ok(re) => Some(re),
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("invalid regex '{regex_str}': {e}"))
                }
            }
        };

        let current_skills = self.skills.get();
        let matches: Vec<&SkillInfo> = current_skills
            .iter()
            .filter(|s| {
                // Hide user-invocable-only skills from listing
                if s.sven_meta.as_ref().is_some_and(|m| m.user_invocable_only) {
                    return false;
                }
                match &compiled_re {
                    None => true,
                    Some(re) => {
                        re.is_match(&s.command)
                            || re.is_match(&s.name)
                            || re.is_match(&s.description)
                    }
                }
            })
            .collect();

        if matches.is_empty() {
            let msg = if regex_str.is_empty() {
                "(no skills available)".to_string()
            } else {
                format!("(no skills matched regex '{regex_str}')")
            };
            return ToolOutput::ok(&call.id, msg);
        }

        let lines: Vec<String> = matches
            .iter()
            .map(|s| {
                let one_liner = s.description.lines().next().unwrap_or("").trim();
                format!(
                    "  <skill command=\"{}\" name=\"{}\">{}</skill>",
                    s.command, s.name, one_liner
                )
            })
            .collect();

        ToolOutput::ok(
            &call.id,
            format!(
                "<skills count=\"{}\">\n{}\n</skills>",
                matches.len(),
                lines.join("\n")
            ),
        )
    }
}

// ── File collection helpers ───────────────────────────────────────────────────

/// Recursively collect file paths under `dir`, excluding `exclude_file`.
fn collect_files_recursive(
    dir: &std::path::Path,
    out: &mut Vec<String>,
    exclude_file: &std::path::Path,
    skip_skill_subdirs: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == exclude_file {
            continue;
        }
        if path.is_dir() {
            if skip_skill_subdirs && path.join("SKILL.md").exists() {
                continue;
            }
            collect_files_recursive(&path, out, exclude_file, skip_skill_subdirs);
        } else if path.is_file() {
            out.push(path.display().to_string());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;
    use serde_json::json;
    use std::path::PathBuf;
    use sven_runtime::{SharedSkills, SkillInfo, SvenSkillMeta};

    fn make_skill(command: &str, description: &str, content: &str) -> SkillInfo {
        let name = command.rsplit('/').next().unwrap_or(command).to_string();
        let skill_dir = PathBuf::from(format!("/tmp/skills/{command}"));
        SkillInfo {
            command: command.to_string(),
            name,
            description: description.to_string(),
            version: None,
            skill_md_path: skill_dir.join("SKILL.md"),
            skill_dir,
            content: content.to_string(),
            sven_meta: None,
        }
    }

    fn make_tool(skills: Vec<SkillInfo>) -> SkillTool {
        SkillTool::new(SharedSkills::new(skills))
    }

    fn load_call(command: &str) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "skill".into(),
            args: json!({ "action": "load", "name": command }),
        }
    }

    fn list_call(regex: Option<&str>) -> ToolCall {
        let args = match regex {
            Some(r) => json!({ "action": "list", "regex": r }),
            None => json!({ "action": "list" }),
        };
        ToolCall {
            id: "t2".into(),
            name: "skill".into(),
            args,
        }
    }

    // ── load action ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn load_existing_skill_returns_content() {
        let tool = make_tool(vec![make_skill(
            "git-workflow",
            "Git helper.",
            "## Steps\n\n1. Run git status.",
        )]);
        let out = tool.execute(&load_call("git-workflow")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("## Steps"));
        assert!(out.content.contains("Base directory:"));
        assert!(out.content.contains("command=\"git-workflow\""));
    }

    #[tokio::test]
    async fn load_nested_skill_by_command_path() {
        let tool = make_tool(vec![make_skill("sven/plan", "Planning phase.", "## Plan")]);
        let out = tool.execute(&load_call("sven/plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("command=\"sven/plan\""));
        assert!(out.content.contains("## Plan"));
    }

    #[tokio::test]
    async fn load_missing_skill_returns_error() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git helper.", "body")]);
        let out = tool.execute(&load_call("nonexistent")).await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"));
        assert!(out.content.contains("git-workflow"));
    }

    #[tokio::test]
    async fn load_missing_name_param_returns_error() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git.", "body")]);
        let call = ToolCall {
            id: "t2".into(),
            name: "skill".into(),
            args: json!({ "action": "load" }),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'name'"));
    }

    #[tokio::test]
    async fn load_parent_shows_hint_for_direct_children() {
        let parent = make_skill(
            "sven",
            "Top-level orchestrator.",
            "## Sven Workflow\n\nFor planning call skill(load, 'sven/plan').",
        );
        let child = make_skill(
            "sven/plan",
            "Planning step — call this when planning.",
            "## Planning detail — this body must NOT appear in parent load.",
        );
        let tool = make_tool(vec![parent, child]);

        let out = tool.execute(&load_call("sven")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Sven Workflow"));
        assert!(out.content.contains("sven/plan"), "child command in hint");
        assert!(
            !out.content
                .contains("Planning detail — this body must NOT appear"),
            "child body must not be embedded"
        );
    }

    #[tokio::test]
    async fn load_skill_content_ends_with_close_tag() {
        let tool = make_tool(vec![make_skill("my-skill", "Desc.", "Content here.")]);
        let out = tool.execute(&load_call("my-skill")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("</skill_content>"));
    }

    // ── list action ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_all_skills() {
        let tool = make_tool(vec![
            make_skill("git-workflow", "Git helper.", "body"),
            make_skill("rust-expert", "Rust coding.", "body"),
        ]);
        let out = tool.execute(&list_call(None)).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("git-workflow"));
        assert!(out.content.contains("rust-expert"));
    }

    #[tokio::test]
    async fn list_with_regex_filters() {
        let tool = make_tool(vec![
            make_skill("git-workflow", "Git helper.", "body"),
            make_skill("rust-expert", "Rust coding.", "body"),
        ]);
        let out = tool.execute(&list_call(Some("rust"))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("rust-expert"));
        assert!(!out.content.contains("git-workflow"));
    }

    #[tokio::test]
    async fn list_regex_matches_description() {
        let tool = make_tool(vec![
            make_skill("git-workflow", "Git branching and commit helper.", "body"),
            make_skill("rust-expert", "Rust coding expert.", "body"),
        ]);
        let out = tool.execute(&list_call(Some("commit"))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("git-workflow"));
        assert!(!out.content.contains("rust-expert"));
    }

    #[tokio::test]
    async fn list_no_match_returns_message() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git helper.", "body")]);
        let out = tool.execute(&list_call(Some("zzznomatch"))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no skills matched"));
    }

    #[tokio::test]
    async fn list_invalid_regex_is_error() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git.", "body")]);
        let out = tool.execute(&list_call(Some("[invalid"))).await;
        assert!(out.is_error);
        assert!(out.content.contains("invalid regex"));
    }

    #[tokio::test]
    async fn list_omits_user_invocable_only_skills() {
        let mut private = make_skill("private", "Private skill.", "body");
        private.sven_meta = Some(SvenSkillMeta {
            user_invocable_only: true,
            ..Default::default()
        });
        let tool = make_tool(vec![make_skill("public", "Public skill.", "body"), private]);
        let out = tool.execute(&list_call(None)).await;
        assert!(!out.is_error);
        assert!(out.content.contains("public"));
        assert!(!out.content.contains("private"));
    }

    #[tokio::test]
    async fn list_empty_store_returns_message() {
        let tool = make_tool(vec![]);
        let out = tool.execute(&list_call(None)).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no skills available"));
    }

    // ── description tests ────────────────────────────────────────────────────

    #[test]
    fn description_lists_non_user_invocable_skills() {
        let mut skill = make_skill("helper", "Help skill.", "body");
        skill.sven_meta = Some(SvenSkillMeta {
            user_invocable_only: false,
            ..Default::default()
        });
        let tool = make_tool(vec![skill]);
        assert!(tool.description().contains("helper"));
    }

    #[test]
    fn description_omits_user_invocable_only_skills() {
        let mut skill = make_skill("private", "Private skill.", "body");
        skill.sven_meta = Some(SvenSkillMeta {
            user_invocable_only: true,
            ..Default::default()
        });
        let tool = make_tool(vec![skill]);
        assert!(!tool.description().contains("private"));
    }

    #[test]
    fn description_includes_command_field() {
        let tool = make_tool(vec![make_skill("sven/plan", "Plan phase.", "body")]);
        let desc = tool.description();
        assert!(
            desc.contains("<command>sven/plan</command>"),
            "description lists command path"
        );
    }
}
