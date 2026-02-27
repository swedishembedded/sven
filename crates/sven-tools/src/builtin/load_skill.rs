// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Tool that loads a named skill's full content into the conversation context.
//!
//! The model calls this tool after recognising that a user request matches one
//! of the skills listed in the system prompt's `<available_skills>` block.
//! The tool returns:
//!
//! - The full SKILL.md body (everything after the frontmatter fence).
//! - The absolute path to the skill directory so the model can resolve bundled
//!   resources (`scripts/`, `references/`, `assets/`) relative to it.
//! - A sampled listing of up to [`MAX_BUNDLED_FILES`] bundled file paths so
//!   the model knows what resources are available without reading them all.
//! - A compact navigation hint listing **direct child sub-skills** (name +
//!   one-line description) when the skill has nested skill packages below it.
//!   Child bodies are never loaded eagerly — the model calls `load_skill`
//!   again for each child when needed.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_runtime::SkillInfo;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Maximum number of bundled file paths to list in the tool response.
const MAX_BUNDLED_FILES: usize = 20;

/// Build the static description string for the tool, listing available skills.
fn build_description(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return "Load a named skill's full instructions into context. \
                No skills are currently available."
            .to_string();
    }

    let skill_list: String = skills
        .iter()
        .filter(|s| !s.sven_meta.as_ref().is_some_and(|m| m.user_invocable_only))
        .map(|s| format!(
            "  <skill>\n    <command>{}</command>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
            s.command,
            s.name,
            s.description.trim()
        ))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Load the full instructions for a named skill into the conversation context.\n\n\
         Call this tool when the user's request matches a skill description. \
         The tool returns the skill's SKILL.md body, the absolute base directory \
         (so relative paths like `scripts/` and `references/` are resolvable), \
         and a listing of bundled files.\n\n\
         Pass the `<command>` value (e.g. \"sven\" or \"sven/plan\") as the `name` argument.\n\n\
         <available_skills>\n{skill_list}\n</available_skills>"
    )
}

/// Return the direct children of `parent` in the skill hierarchy.
///
/// A skill is a direct child when its command equals `parent.command + "/" + X`
/// where `X` contains no further `/`.
fn direct_children<'a>(parent: &SkillInfo, all: &'a [SkillInfo]) -> Vec<&'a SkillInfo> {
    let prefix = format!("{}/", parent.command);
    all.iter()
        .filter(|s| {
            s.command.starts_with(&prefix)
                && !s.command[prefix.len()..].contains('/')
        })
        .collect()
}

/// Build a compact `<sub_skills>` navigation block listing direct child skills.
///
/// Only the child's **command and one-line description** are included — not the
/// full body.  The model must call `load_skill("<command>")` to load any child's
/// full instructions when it is actually needed.
fn build_sub_skills_hint(parent: &SkillInfo, all: &[SkillInfo]) -> String {
    let children = direct_children(parent, all);
    if children.is_empty() {
        return String::new();
    }

    let lines: Vec<String> = children
        .iter()
        .map(|child| {
            let one_liner = child.description.lines().next().unwrap_or("").trim();
            format!("  <sub_skill command=\"{}\" name=\"{}\">{}</sub_skill>",
                child.command, child.name, one_liner)
        })
        .collect();

    format!(
        "\n\n<sub_skills>\n\
         <!-- Call load_skill(command) to load any sub-skill's full instructions. -->\n\
         {}\n\
         </sub_skills>",
        lines.join("\n")
    )
}

/// Tool that loads a named skill's full content on demand.
///
/// Construct with [`LoadSkillTool::new`] to pre-compute the description string.
pub struct LoadSkillTool {
    /// Shared skill list (discovered once at startup).
    skills: Arc<[SkillInfo]>,
    /// Pre-computed description (includes available-skills XML).
    description: String,
}

impl LoadSkillTool {
    /// Create a new `LoadSkillTool` from a shared skill slice.
    pub fn new(skills: Arc<[SkillInfo]>) -> Self {
        let description = build_description(&skills);
        Self { skills, description }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str { "load_skill" }

    fn description(&self) -> &str { &self.description }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The command of the skill to load (e.g. \"sven\" or \"sven/plan\")"
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'name' parameter"),
        };

        debug!(skill = %command, "load_skill tool");

        let skill = match self.skills.iter().find(|s| s.command == command) {
            Some(s) => s,
            None => {
                let available = self
                    .skills
                    .iter()
                    .map(|s| s.command.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "skill \"{command}\" not found. Available skills: {}",
                        if available.is_empty() { "(none)" } else { &available }
                    ),
                );
            }
        };

        // Collect bundled file paths (up to MAX_BUNDLED_FILES), sorted for
        // determinism, excluding the SKILL.md file itself and any sub-skill
        // SKILL.md files (they belong to their own skill packages).
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

        // Add a compact navigation hint listing direct sub-skills.  The model
        // uses this to know which child skills exist and when to call
        // load_skill() for them — without loading their bodies now.
        let sub_skills_hint = build_sub_skills_hint(skill, &self.skills);

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
}

// ── File collection helpers ───────────────────────────────────────────────────

/// Recursively collect file paths under `dir`, excluding `exclude_file`.
///
/// When `skip_skill_subdirs` is `true`, subdirectories that contain their own
/// `SKILL.md` are not descended into — they are separate skill packages with
/// their own file listings.
fn collect_files_recursive(
    dir: &std::path::Path,
    out: &mut Vec<String>,
    exclude_file: &std::path::Path,
    skip_skill_subdirs: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == exclude_file {
            continue;
        }
        if path.is_dir() {
            // Skip sub-skill directories — they are separate packages.
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
    use sven_runtime::{SkillInfo, SvenSkillMeta};
    use std::path::PathBuf;

    fn make_skill(command: &str, description: &str, content: &str) -> SkillInfo {
        // Name falls back to the last segment of the command.
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

    fn make_tool(skills: Vec<SkillInfo>) -> LoadSkillTool {
        LoadSkillTool::new(Arc::from(skills.into_boxed_slice()))
    }

    fn call(command: &str) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "load_skill".into(),
            args: json!({ "name": command }),
        }
    }

    #[tokio::test]
    async fn load_existing_skill_returns_content() {
        let tool = make_tool(vec![make_skill(
            "git-workflow",
            "Git helper.",
            "## Steps\n\n1. Run git status.",
        )]);
        let out = tool.execute(&call("git-workflow")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("## Steps"));
        assert!(out.content.contains("Base directory:"));
        assert!(out.content.contains("command=\"git-workflow\""));
    }

    #[tokio::test]
    async fn load_nested_skill_by_command_path() {
        let tool = make_tool(vec![make_skill(
            "sven/plan",
            "Planning phase.",
            "## Plan",
        )]);
        let out = tool.execute(&call("sven/plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("command=\"sven/plan\""));
        assert!(out.content.contains("## Plan"));
    }

    #[tokio::test]
    async fn load_missing_skill_returns_error() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git helper.", "body")]);
        let out = tool.execute(&call("nonexistent")).await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"));
        assert!(out.content.contains("git-workflow"));
    }

    #[tokio::test]
    async fn load_skill_missing_name_param_returns_error() {
        let tool = make_tool(vec![make_skill("git-workflow", "Git.", "body")]);
        let no_name_call = ToolCall {
            id: "t2".into(),
            name: "load_skill".into(),
            args: json!({}),
        };
        let out = tool.execute(&no_name_call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'name'"));
    }

    #[test]
    fn description_lists_non_user_invocable_skills() {
        let mut skill = make_skill("helper", "Help skill.", "body");
        skill.sven_meta = Some(SvenSkillMeta { user_invocable_only: false, ..Default::default() });
        let tool = make_tool(vec![skill]);
        assert!(tool.description().contains("helper"));
    }

    #[test]
    fn description_omits_user_invocable_only_skills() {
        let mut skill = make_skill("private", "Private skill.", "body");
        skill.sven_meta = Some(SvenSkillMeta { user_invocable_only: true, ..Default::default() });
        let tool = make_tool(vec![skill]);
        assert!(!tool.description().contains("private"));
    }

    #[test]
    fn description_with_no_skills_mentions_unavailable() {
        let tool = make_tool(vec![]);
        assert!(tool.description().contains("No skills"));
    }

    #[tokio::test]
    async fn load_skill_content_ends_with_close_tag() {
        let tool = make_tool(vec![make_skill("my-skill", "Desc.", "Content here.")]);
        let out = tool.execute(&call("my-skill")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("</skill_content>"));
    }

    // ── Hierarchical skill tests (directory-structure-based) ──────────────────
    //
    // Sub-skill relationships are derived from command path prefixes.
    // The tool provides a compact navigation hint for direct children.
    // Child bodies are never loaded eagerly.

    #[tokio::test]
    async fn load_parent_shows_hint_for_direct_children() {
        let parent = make_skill(
            "sven",
            "Top-level orchestrator.",
            "## Sven Workflow\n\nFor planning call load_skill('sven/plan').",
        );
        let child = make_skill(
            "sven/plan",
            "Planning step — call this when planning.",
            "## Planning detail — this body must NOT appear in parent load.",
        );
        let tool = make_tool(vec![parent, child]);

        let out = tool.execute(&call("sven")).await;
        assert!(!out.is_error, "{}", out.content);
        // Parent body present
        assert!(out.content.contains("Sven Workflow"));
        // Child command referenced in the hint block
        assert!(out.content.contains("sven/plan"), "child command in hint");
        // The navigation hint mentions calling load_skill
        assert!(out.content.contains("load_skill"), "hint mentions load_skill");
        // Child full body must NOT appear
        assert!(
            !out.content.contains("Planning detail — this body must NOT appear"),
            "child body must not be embedded"
        );
    }

    #[tokio::test]
    async fn load_parent_hint_uses_one_line_description() {
        let parent = make_skill("sven", "Orchestrator.", "Parent body.");
        let child = make_skill("sven/plan", "Planning step — call this when planning.", "Full plan body.");
        let tool = make_tool(vec![parent, child]);

        let out = tool.execute(&call("sven")).await;
        assert!(!out.is_error);
        // The one-liner description should appear in the hint
        assert!(out.content.contains("Planning step"), "description one-liner in hint");
        // Full body must not appear
        assert!(!out.content.contains("Full plan body."), "full body must not be embedded");
    }

    #[tokio::test]
    async fn load_parent_hint_excludes_grandchildren() {
        // Grandchildren are not direct children — they should NOT appear in the
        // parent's hint.  They appear in the child's hint instead.
        let parent   = make_skill("sven", "Orchestrator.", "Parent body.");
        let child    = make_skill("sven/implement", "Impl phase.", "Impl body.");
        let grandchild = make_skill("sven/implement/research", "Research.", "Research body.");
        let tool = make_tool(vec![parent, child, grandchild]);

        let parent_out = tool.execute(&call("sven")).await;
        assert!(!parent_out.content.contains("research"), "grandchild should not appear in parent hint");
        assert!(parent_out.content.contains("sven/implement"), "direct child in parent hint");

        let child_out = tool.execute(&call("sven/implement")).await;
        assert!(child_out.content.contains("research"), "grandchild in child hint");
    }

    #[tokio::test]
    async fn load_skill_no_children_has_no_sub_skills_block() {
        let tool = make_tool(vec![make_skill("simple", "Simple.", "Just content.")]);
        let out = tool.execute(&call("simple")).await;
        assert!(!out.is_error);
        assert!(!out.content.contains("<sub_skills>"), "no sub_skills block without children");
    }

    #[tokio::test]
    async fn load_skill_multi_step_workflow_hint_only() {
        // All three children appear as hints; none of their bodies appear.
        let parent = make_skill(
            "sven",
            "Full workflow.",
            "## Workflow\n\nPlan: load_skill('sven/plan'). Impl: load_skill('sven/implement').",
        );
        let plan = make_skill("sven/plan", "Planning phase.", "## Plan body — must not appear");
        let imp  = make_skill("sven/implement", "Implementation phase.", "## Impl body — must not appear");
        let rev  = make_skill("sven/review", "Review phase.", "## Review body — must not appear");
        let tool = make_tool(vec![parent, plan, imp, rev]);

        let out = tool.execute(&call("sven")).await;
        assert!(!out.is_error);
        // Commands appear in hints
        assert!(out.content.contains("sven/plan"));
        assert!(out.content.contains("sven/implement"));
        assert!(out.content.contains("sven/review"));
        // Descriptions (one-liners) appear in hints
        assert!(out.content.contains("Planning phase."));
        assert!(out.content.contains("Implementation phase."));
        assert!(out.content.contains("Review phase."));
        // Full bodies must NOT appear
        assert!(!out.content.contains("must not appear"), "no full child bodies embedded");
    }

    #[tokio::test]
    async fn description_includes_command_field() {
        let tool = make_tool(vec![make_skill("sven/plan", "Plan phase.", "body")]);
        let desc = tool.description();
        assert!(desc.contains("<command>sven/plan</command>"), "description lists command path");
    }
}
