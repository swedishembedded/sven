// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::path::Path;
use std::sync::Arc;

use sven_config::AgentMode;
use sven_runtime::{AgentInfo, SkillInfo};

/// All optional contextual blocks that can be injected into the system prompt.
#[derive(Debug)]
pub struct PromptContext<'a> {
    /// Absolute path to the project root (from `.git` detection).
    pub project_root: Option<&'a Path>,
    /// Pre-formatted git context (branch, commit, dirty status).
    ///
    /// **Caching note**: this field is *volatile* — it changes on every commit
    /// and with every file edit (dirty count).  When prompt caching is enabled
    /// this content is placed in a *separate, uncached* system block so that
    /// the stable prefix remains cacheable across sessions.
    pub git_context: Option<&'a str>,
    /// Contents of the project context file (AGENTS.md / .sven/context.md).
    pub project_context_file: Option<&'a str>,
    /// Pre-formatted CI environment block.
    ///
    /// **Caching note**: like `git_context`, this is volatile between CI runs.
    pub ci_context: Option<&'a str>,
    /// Text appended verbatim after the default Guidelines section.
    pub append: Option<&'a str>,
    /// Discovered skills.  Metadata (name + description) is injected into the
    /// stable system prompt so the model always knows what skills are available.
    /// Held as an `Arc` so a fresh snapshot can be taken from [`SharedSkills`]
    /// on each turn without cloning the skill data.
    pub skills: Arc<[SkillInfo]>,
    /// Discovered subagents.  Names and descriptions are injected into the
    /// stable system prompt so the model can suggest delegation and the user
    /// can invoke them via slash commands.
    pub agents: Arc<[AgentInfo]>,
}

impl<'a> Default for PromptContext<'a> {
    fn default() -> Self {
        Self {
            project_root: None,
            git_context: None,
            project_context_file: None,
            ci_context: None,
            append: None,
            skills: Arc::from(Vec::<SkillInfo>::new()),
            agents: Arc::from(Vec::<AgentInfo>::new()),
        }
    }
}

impl<'a> PromptContext<'a> {
    /// Return a version of this context with the volatile fields cleared.
    ///
    /// Used to build the *stable* (cacheable) portion of the system prompt.
    /// Skills and agents are stable within a session (discovered once at
    /// startup) so they are included in the stable slice.
    pub fn stable_only(&self) -> Self {
        Self {
            project_root: self.project_root,
            git_context: None,
            project_context_file: self.project_context_file,
            ci_context: None,
            append: self.append,
            skills: self.skills.clone(),
            agents: self.agents.clone(),
        }
    }

    /// Format the volatile fields (git + CI context) as a block suitable for
    /// appending to the system prompt outside the cached region.
    ///
    /// Returns `None` when neither git nor CI context is present.
    pub fn dynamic_block(&self) -> Option<String> {
        let git = self.git_context
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        let ci = self.ci_context
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        match (git, ci) {
            (None, None) => None,
            (Some(g), None) => Some(g),
            (None, Some(c)) => Some(c),
            (Some(g), Some(c)) => Some(format!("{g}\n\n{c}")),
        }
    }
}

// ─── Guidelines Module ───────────────────────────────────────────────────────
// Modular guidelines for easier maintenance and testing

mod guidelines {
    pub fn general() -> &'static str {
        "- Be concise and precise. Use tools instead of guessing — always verify tool outputs.\n\
         - When a task is ambiguous or you need information to proceed, use the `ask_question` \
           tool to collect structured answers from the user rather than making assumptions or \
           writing a prose question. The `ask_question` tool presents a modal dialog in the TUI; \
           prefer it over free-form text questions whenever the user is interactive.\n\
         - Check `update_memory` (list) at session start for stored project context."
    }

    pub fn tool_usage() -> &'static str {
        "- NEVER use `run_terminal_command` for file I/O — use `read_file`/`write`/`edit_file`/`grep`/`glob`.\n\
         - Prefer `edit_file` over `write` for modifying existing files (preserves surrounding context).\n\
         - Discovery workflow: `glob` to find files → `grep` with output_mode='files_with_matches' to narrow → `read_file` for full context.\n\
         - Use `grep` output_mode='content' + context_lines for code-level inspection; use `search_codebase` for broad whole-repo sweeps.\n\
         - Use `edit_file` with replace_all=true to rename a symbol throughout a single file.\n\
         - Batch `read_file` calls in parallel — read all potentially relevant files in one turn."
    }

    pub fn code_quality() -> &'static str {
        "- Follow existing project conventions discovered via file analysis before writing any code.\n\
         - NEVER create new files proactively unless explicitly requested.\n\
         - Run `read_lints` (scoped to edited files) after every substantive code change.\n\
         - Include tests when adding new functionality; preserve existing test coverage.\n\
         - Preserve existing code structure and naming patterns."
    }

    pub fn workflow_efficiency() -> &'static str {
        "- Use `todo_write` for multi-step tasks (3+ steps); update silently and mark complete immediately.\n\
         - Use `switch_mode` to transition between Research, Plan, and Agent modes proactively.\n\
         - Store project-specific conventions in `update_memory`; retrieve them at the start of new sessions.\n\
         - Batch independent tool calls in parallel to reduce round-trips."
    }

    pub fn error_handling() -> &'static str {
        "- When a tool fails, read the error message carefully and adjust your approach.\n\
         - `edit_file` tries exact → strip-prefixes → indent-normalized → fuzzy automatically.\n\
         - `edit_file` 'not found': all strategies failed — check the 'Most similar sections' in\n\
           the error to see the actual file content, then copy old_str from there.\n\
           Re-read the file after each successful edit before making the next one.\n\
         - `edit_file` 'N times': add more unique surrounding context, or use replace_all=true.\n\
         - Always set `workdir` in `run_terminal_command` to project_root for commands that depend on location.\n\
         - NEVER skip git hooks or force-push without explicit user permission."
    }

    pub fn debugging() -> &'static str {
        "- When asked to debug, diagnose a crash, inspect runtime state, or step through code on a \
           target device: you MUST use the GDB tools (`gdb_start_server`, `gdb_connect`, \
           `gdb_command`, `gdb_interrupt`, `gdb_stop`). \
           Do NOT substitute reading source code for actual runtime debugging — source reading \
           answers 'what does the code say', GDB answers 'what is the program actually doing'.\n\
         - Lifecycle: `gdb_start_server` (or skip if server already running) → `gdb_connect` → \
           `gdb_command` (load / break / continue / step / info registers / backtrace) → `gdb_stop`.\n\
         - If `gdb_start_server` reports a server is already running, call `gdb_connect` directly.\n\
         - Use `gdb_wait_stopped` after `continue` or `step` commands before issuing the next command."
    }
}

// ─── Skills section ───────────────────────────────────────────────────────────

/// Maximum total characters for the `<available_skills>` block in the system
/// prompt.  Binary search is used to fit within this budget when there are many
/// skills.
pub const MAX_SKILLS_PROMPT_CHARS: usize = 30_000;

/// Format the available-skills block for injection into the system prompt.
///
/// Skills with `user_invocable_only: true` are omitted (they are still
/// registered as TUI slash commands but not shown to the model).
/// Skills with `always: true` bypass the char-budget check and are always
/// included.
///
/// Returns an empty string when `skills` is empty.
pub fn build_skills_section(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let always_skills: Vec<&SkillInfo> = skills
        .iter()
        .filter(|s| s.sven_meta.as_ref().is_some_and(|m| m.always))
        .collect();

    let candidate_skills: Vec<&SkillInfo> = skills
        .iter()
        .filter(|s| {
            let is_always = s.sven_meta.as_ref().is_some_and(|m| m.always);
            let user_only = s.sven_meta.as_ref().is_some_and(|m| m.user_invocable_only);
            !user_only && !is_always
        })
        .collect();

    // Build XML entries for always-included skills.
    let always_entries: Vec<String> = always_skills
        .iter()
        .map(|s| format!(
            "  <skill>\n    <command>{}</command>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
            s.command,
            s.name,
            s.description.trim()
        ))
        .collect();

    let remaining_budget = MAX_SKILLS_PROMPT_CHARS
        .saturating_sub(always_entries.iter().map(|e| e.len()).sum::<usize>());

    let candidate_entries: Vec<String> = candidate_skills
        .iter()
        .map(|s| format!(
            "  <skill>\n    <command>{}</command>\n    <name>{}</name>\n    <description>{}</description>\n  </skill>",
            s.command,
            s.name,
            s.description.trim()
        ))
        .collect();

    // Walk forward through candidate entries, accumulating size, and stop once the
    // budget would be exceeded.  Skills are bounded in practice (< a few hundred),
    // so a linear scan is both correct and efficient.
    let mut used = 0usize;
    let fitted_count = candidate_entries
        .iter()
        .take_while(|e| {
            let next = used + e.len();
            if next <= remaining_budget {
                used = next;
                true
            } else {
                false
            }
        })
        .count();
    let fitted_entries = &candidate_entries[..fitted_count];

    let all_entries: Vec<&str> = always_entries
        .iter()
        .chain(fitted_entries.iter())
        .map(String::as_str)
        .collect();

    if all_entries.is_empty() {
        return String::new();
    }

    let truncated = fitted_count < candidate_entries.len();
    let truncation_note = if truncated {
        format!(
            "\n⚠ Skills truncated: showing {} of {}.",
            fitted_count + always_entries.len(),
            skills.len()
        )
    } else {
        String::new()
    };

    format!(
        "## Skills\n\n\
         When you recognize that the current task matches one of the available skills listed \
         below, call the `load_skill` tool to load the full skill instructions before \
         proceeding.  Match the skill `<description>` trigger phrases against the user's \
         request.  Load at most one skill per task; do not load a skill unless it clearly \
         applies.\
         {truncation_note}\n\n\
         <available_skills>\n{}\n</available_skills>",
        all_entries.join("\n")
    )
}

// ─── Agents section ───────────────────────────────────────────────────────────

/// Maximum total characters for the `<available_agents>` block.
pub const MAX_AGENTS_PROMPT_CHARS: usize = 10_000;

/// Format the available-agents block for injection into the system prompt.
///
/// Returns an empty string when `agents` is empty.
pub fn build_agents_section(agents: &[AgentInfo]) -> String {
    if agents.is_empty() {
        return String::new();
    }

    let entries: Vec<String> = agents
        .iter()
        .map(|a| {
            let model_hint = match a.model.as_deref() {
                Some(m) => format!("\n    <model>{m}</model>"),
                None => String::new(),
            };
            let bg_hint = if a.is_background { "\n    <background>true</background>" } else { "" };
            let ro_hint = if a.readonly { "\n    <readonly>true</readonly>" } else { "" };
            format!(
                "  <agent>\n    <name>{}</name>\n    <description>{}</description>{}{}{}\n  </agent>",
                a.name,
                a.description.trim(),
                model_hint,
                bg_hint,
                ro_hint,
            )
        })
        .collect();

    // Fit entries within budget.
    let mut used = 0usize;
    let fitted_count = entries
        .iter()
        .take_while(|e| {
            let next = used + e.len();
            if next <= MAX_AGENTS_PROMPT_CHARS {
                used = next;
                true
            } else {
                false
            }
        })
        .count();

    if fitted_count == 0 {
        return String::new();
    }

    let fitted = &entries[..fitted_count];
    let truncation_note = if fitted_count < entries.len() {
        format!(
            "\n⚠ Agents truncated: showing {} of {}.",
            fitted_count,
            agents.len()
        )
    } else {
        String::new()
    };

    format!(
        "## Subagents\n\n\
         The following subagents are available for delegation.  When the user's task \
         clearly matches a subagent's description, suggest invoking it explicitly with \
         a slash command (e.g. `/verifier confirm the auth flow`).  Users can also \
         invoke subagents directly by typing `/<name> <task>` in the input box.\
         {truncation_note}\n\n\
         <available_agents>\n{}\n</available_agents>",
        fitted.join("\n")
    )
}

fn build_guidelines_section() -> String {
    format!(
        "## Guidelines\n\n\
         ### General Principles\n\
         {}\n\n\
         ### Tool Usage Patterns\n\
         {}\n\n\
         ### Code Quality\n\
         {}\n\n\
         ### Workflow Efficiency\n\
         {}\n\n\
         ### Error Handling\n\
         {}\n\n\
         ### Debugging\n\
         {}",
        guidelines::general(),
        guidelines::tool_usage(),
        guidelines::code_quality(),
        guidelines::workflow_efficiency(),
        guidelines::error_handling(),
        guidelines::debugging()
    )
}

fn build_tool_examples_section() -> &'static str {
    "## Tool Usage Examples\n\n\
     Example 1: Locate and modify a function\n\
     <example>\n\
     1. grep: pattern=\"fn process_data\", output_mode=\"files_with_matches\" → Discover which files contain it\n\
     2. read_file: path=\"/project/src/processor.rs\" → Read full file for context\n\
     3. edit_file: old_str=\"<lines from the file — strip Ln: prefix if copied from read_file>\", new_str=\"...\" → Apply change\n\
     Note: edit_file auto-corrects minor indent/prefix differences; on failure it shows 'Most similar sections'.\n\
     4. read_lints: paths=[\"/project/src/processor.rs\"] → Verify no new errors\n\
     </example>\n\n\
     Example 2: Rename a symbol across a file\n\
     <example>\n\
     edit_file: path=\"src/lib.rs\", old_str=\"OldName\", new_str=\"NewName\", replace_all=true\n\
     Then: read_lints to confirm no type errors introduced.\n\
     </example>\n\n\
     Example 3: Parallel exploration before making changes\n\
     <example>\n\
     In one turn, call in parallel:\n\
     - read_file: path=\"/project/src/main.rs\"\n\
     - read_file: path=\"/project/Cargo.toml\"\n\
     - grep: pattern=\"TODO|FIXME\", output_mode=\"files_with_matches\"\n\
     - update_memory: operation=\"list\"  (check stored project context)\n\
     </example>\n\n\
     Example 4: Deep code inspection with context\n\
     <example>\n\
     grep: pattern=\"unsafe\", include=\"*.rs\", output_mode=\"content\", context_lines=3\n\
     → See 3 lines of surrounding code for each match to understand usage.\n\
     </example>\n\n\
     Example 5: Research → Plan → Implement workflow\n\
     <example>\n\
     1. switch_mode: mode=\"research\"\n\
     2. search_codebase: query=\"authentication\" → Broad sweep of auth-related files\n\
     3. grep: pattern=\"fn (login|authenticate)\", include=\"*.rs\", output_mode=\"files_with_matches\"\n\
     4. read_file: (all relevant files in parallel)\n\
     5. switch_mode: mode=\"plan\" → Design the approach\n\
     6. switch_mode: mode=\"agent\" → Implement\n\
     7. run_terminal_command: command=\"cargo test\" → Verify\n\
     </example>"
}

/// Build the system prompt for the given agent mode.
///
/// `tool_names` lists the tools available in this mode so the model knows
/// exactly what it can use.  `ctx` carries optional project / CI / git
/// context injected when running in headless mode.
pub fn system_prompt(
    mode: AgentMode,
    custom: Option<&str>,
    tool_names: &[String],
    ctx: PromptContext<'_>,
) -> String {
    if let Some(custom) = custom {
        // Even with a custom prompt, honour append if set.
        if let Some(extra) = ctx.append {
            return format!("{}\n\n{}", custom.trim_end(), extra);
        }
        return custom.to_string();
    }

    // Enhanced agent identity highlighting Sven's unique features
    let agent_identity = format!(
        "You are Sven, a specialized AI coding agent built for professional software development.\n\n\
         Operating Mode: `{mode}`\n\n\
         Core Capabilities:\n\
         - Multi-mode operation (Research, Plan, Agent) with dynamic mode switching\n\
         - Persistent memory across sessions via `update_memory` tool\n\
         - Integrated debugging support with GDB tools\n\
         - Markdown-driven workflows with frontmatter configuration\n\
         - Comprehensive linting and test integration\n\
         - Full CI/CD pipeline integration and awareness"
    );

    let mode_instructions = match mode {
        AgentMode::Research => {
            "You are a research assistant.  You may read files, search the codebase, and look up \
             information.  You MUST NOT write, modify, or delete any files.  Focus on \
             gathering and summarising information accurately."
        }
        AgentMode::Plan => {
            "You are a planning assistant.  Analyse the request and produce a clear, structured \
             plan with numbered steps.  You may read files to inform the plan, but MUST NOT \
             modify them.  Output the plan in Markdown."
        }
        AgentMode::Agent => {
            "You are a capable coding agent.  You can read and write files, run shell commands, \
             and search the codebase.  Work systematically, verify your changes, and report \
             your progress clearly."
        }
    };

    let tools_section = if tool_names.is_empty() {
        String::new()
    } else {
        let list = tool_names.iter()
            .map(|n| format!("- `{n}`"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n## Available Tools\n{list}")
    };

    let tool_examples_section = format!("\n\n{}", build_tool_examples_section());

    let project_section = if let Some(root) = ctx.project_root {
        format!(
            "\n\n## Project Context\n\
             Project root directory: `{}`\n\
             - Use this absolute path for all file read/write operations.\n\
             - Pass this path as the `workdir` argument to `run_terminal_command` \
               so shell commands execute in the correct directory.\n\
             - Prefer absolute paths over relative paths in every tool call.",
            root.display()
        )
    } else {
        String::new()
    };

    let git_section = if let Some(git) = ctx.git_context {
        format!("\n\n{git}")
    } else {
        String::new()
    };

    // Project context file (AGENTS.md / .sven/context.md) — injected as a
    // labelled section so the model treats it as authoritative instructions.
    let context_file_section = if let Some(content) = ctx.project_context_file {
        format!("\n\n## Project Instructions\n\n{content}")
    } else {
        String::new()
    };

    let ci_section = if let Some(ci) = ctx.ci_context {
        format!("\n\n{ci}")
    } else {
        String::new()
    };

    // Skills — stable, injected after project instructions and before CI/git.
    let skills_section = {
        let s = build_skills_section(&ctx.skills);
        if s.is_empty() { String::new() } else { format!("\n\n{s}") }
    };

    // Agents — stable, injected after skills.
    let agents_section = {
        let s = build_agents_section(&ctx.agents);
        if s.is_empty() { String::new() } else { format!("\n\n{s}") }
    };

    let guidelines_section = build_guidelines_section();

    let append_section = if let Some(extra) = ctx.append {
        format!("\n\n{extra}")
    } else {
        String::new()
    };

    format!(
        "{agent_identity}\n\n\
         {mode_instructions}{tools_section}{tool_examples_section}{project_section}{git_section}\
         {context_file_section}{skills_section}{agents_section}{ci_section}\n\n\
         {guidelines_section}\
         {append_section}",
    )
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use sven_config::AgentMode;

    fn no_tools() -> Vec<String> { vec![] }
    fn p(s: &str) -> PathBuf { PathBuf::from(s) }
    fn empty() -> PromptContext<'static> { PromptContext::default() }

    #[test]
    fn custom_prompt_is_returned_verbatim() {
        let prompt = system_prompt(AgentMode::Agent, Some("Custom instructions here."), &no_tools(), empty());
        assert_eq!(prompt, "Custom instructions here.");
    }

    #[test]
    fn custom_prompt_with_append() {
        let ctx = PromptContext { append: Some("Extra rule."), ..Default::default() };
        let prompt = system_prompt(AgentMode::Agent, Some("Base."), &no_tools(), ctx);
        assert!(prompt.contains("Base."));
        assert!(prompt.contains("Extra rule."));
    }

    #[test]
    fn research_mode_mentions_read_only() {
        let pr = system_prompt(AgentMode::Research, None, &no_tools(), empty());
        assert!(pr.contains("read-only") || pr.contains("MUST NOT write"),
            "Research mode should forbid writes");
    }

    #[test]
    fn plan_mode_mentions_structured_plan() {
        let pr = system_prompt(AgentMode::Plan, None, &no_tools(), empty());
        assert!(pr.to_lowercase().contains("plan"),
            "Plan mode prompt should mention 'plan'");
    }

    #[test]
    fn agent_mode_mentions_write_capability() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("write files") || pr.contains("read and write"),
            "Agent mode should mention write capability");
    }

    #[test]
    fn all_modes_name_sven() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, &no_tools(), empty());
            assert!(pr.contains("Sven"), "prompt should identify the agent as Sven");
        }
    }

    #[test]
    fn all_modes_include_mode_name_in_prompt() {
        for (mode, expected) in [
            (AgentMode::Research, "research"),
            (AgentMode::Plan, "plan"),
            (AgentMode::Agent, "agent"),
        ] {
            let pr = system_prompt(mode, None, &no_tools(), empty());
            assert!(pr.contains(expected),
                "prompt for {mode} should contain the mode name");
        }
    }

    #[test]
    fn all_modes_include_guidelines_section() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, &no_tools(), empty());
            assert!(pr.contains("Guidelines"), "prompt should contain a Guidelines section");
        }
    }

    #[test]
    fn guidelines_include_debugging_section_with_gdb() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("Debugging"), "prompt should contain a Debugging section");
        assert!(pr.contains("gdb_start_server"), "debugging section must mention gdb_start_server");
        assert!(pr.contains("gdb_connect"), "debugging section must mention gdb_connect");
        assert!(
            pr.contains("MUST use the GDB tools") || pr.contains("you MUST use the GDB tools"),
            "debugging section must mandate GDB tool use"
        );
    }

    #[test]
    fn debugging_guideline_present_in_all_modes() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, &no_tools(), empty());
            assert!(pr.contains("gdb_connect"),
                "mode {mode} prompt should mention gdb_connect in debugging guideline");
        }
    }

    #[test]
    fn tools_list_appears_in_prompt() {
        let tools = vec!["read_file".to_string(), "grep".to_string()];
        let pr = system_prompt(AgentMode::Research, None, &tools, empty());
        assert!(pr.contains("`read_file`"));
        assert!(pr.contains("`grep`"));
    }

    #[test]
    fn project_root_appears_in_prompt() {
        let root = p("/home/user/my-project");
        let ctx = PromptContext { project_root: Some(&root), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(pr.contains("/home/user/my-project"),
            "project root should appear in prompt");
        assert!(pr.contains("Project Context"),
            "prompt should have Project Context section");
    }

    #[test]
    fn no_project_root_no_section() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(!pr.contains("Project Context"));
    }

    #[test]
    fn ci_context_is_appended() {
        let ci = "## CI Environment\nRunning in: GitHub Actions\nBranch: main";
        let ctx = PromptContext { ci_context: Some(ci), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(pr.contains("GitHub Actions"));
        assert!(pr.contains("Branch: main"));
    }

    #[test]
    fn git_context_appears_in_prompt() {
        let git = "## Git Context\nBranch: main\nCommit: abc1234";
        let ctx = PromptContext { git_context: Some(git), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(pr.contains("Git Context"));
        assert!(pr.contains("abc1234"));
    }

    #[test]
    fn project_context_file_appears_in_prompt() {
        let file_content = "Always write tests for every function.";
        let ctx = PromptContext { project_context_file: Some(file_content), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(pr.contains("Project Instructions"));
        assert!(pr.contains("Always write tests"));
    }

    #[test]
    fn append_section_is_added_after_guidelines() {
        let ctx = PromptContext { append: Some("Custom rule: never delete files."), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        let guidelines_pos = pr.find("Guidelines").unwrap();
        let append_pos = pr.find("Custom rule").unwrap();
        assert!(append_pos > guidelines_pos, "append should come after Guidelines");
    }

    #[test]
    fn enhanced_agent_identity_mentions_core_capabilities() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("specialized AI coding agent"), "identity should emphasize specialization");
        assert!(pr.contains("Core Capabilities"), "should list core capabilities");
        assert!(pr.contains("Multi-mode operation"), "should mention mode switching");
        assert!(pr.contains("Persistent memory"), "should mention memory feature");
        assert!(pr.contains("GDB tools"), "should mention debugging support");
    }

    #[test]
    fn guidelines_section_has_multiple_categories() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("### General Principles"), "guidelines should have General Principles");
        assert!(pr.contains("### Tool Usage Patterns"), "guidelines should have Tool Usage Patterns");
        assert!(pr.contains("### Code Quality"), "guidelines should have Code Quality");
        assert!(pr.contains("### Workflow Efficiency"), "guidelines should have Workflow Efficiency");
        assert!(pr.contains("### Error Handling"), "guidelines should have Error Handling");
    }

    #[test]
    fn guidelines_section_contains_minimum_items() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        // Count bullet points in guidelines section. Guidelines are rendered with
        // Rust \n\ line continuations so each bullet starts with "\n- " (no indent).
        let guidelines_section = pr.split("## Guidelines").nth(1).unwrap();
        let bullet_count = guidelines_section.matches("\n- ").count();
        assert!(bullet_count >= 15, "guidelines should contain at least 15 bullet points, found {}", bullet_count);
    }

    #[test]
    fn guidelines_mention_critical_tools() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("`run_terminal_command`"), "guidelines should mention run_terminal_command");
        assert!(pr.contains("`edit_file`"), "guidelines should mention edit_file");
        assert!(pr.contains("`grep`"), "guidelines should mention grep");
        assert!(pr.contains("`glob`"), "guidelines should mention glob");
        assert!(pr.contains("`read_file`"), "guidelines should mention read_file");
    }

    #[test]
    fn prompt_contains_tool_usage_examples() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("## Tool Usage Examples"), "should have Tool Usage Examples section");
        assert!(pr.contains("<example>"), "examples should use <example> tags");
        assert!(pr.contains("Example 1:"), "should have multiple examples");
        assert!(pr.contains("Example 2:"), "should have multiple examples");
        assert!(pr.contains("Example 3:"), "should have multiple examples");
    }

    #[test]
    fn guidelines_include_git_safety_warning() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("NEVER") || pr.contains("never skip"), "guidelines should include safety warnings");
    }

    #[test]
    fn guidelines_mention_parallel_operations() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("parallel"), "guidelines should mention parallel tool usage");
    }

    #[test]
    fn guidelines_mention_mode_switching() {
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), empty());
        assert!(pr.contains("switch_mode"), "guidelines should mention mode switching");
        assert!(pr.contains("Research, Plan, and Agent"), "guidelines should list all modes");
    }

    #[test]
    fn all_modes_have_enhanced_identity() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, &no_tools(), empty());
            assert!(pr.contains("specialized AI coding agent"), "all modes should use enhanced identity");
            assert!(pr.contains("Core Capabilities"), "all modes should list capabilities");
        }
    }

    // ── Skills section tests ──────────────────────────────────────────────────

    fn make_test_skill(command: &str, description: &str) -> sven_runtime::SkillInfo {
        use std::path::PathBuf;
        let name = command.rsplit('/').next().unwrap_or(command).to_string();
        sven_runtime::SkillInfo {
            command: command.to_string(),
            name,
            description: description.to_string(),
            version: None,
            skill_md_path: PathBuf::from(format!("/tmp/{command}/SKILL.md")),
            skill_dir: PathBuf::from(format!("/tmp/{command}")),
            content: format!("## {command} content"),
            sven_meta: None,
        }
    }

    #[test]
    fn system_prompt_includes_skills_section_when_skills_provided() {
        let skills = vec![
            make_test_skill("git-workflow", "Use when the user asks about git."),
        ];
        let ctx = PromptContext { skills: Arc::from(skills.into_boxed_slice()), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(pr.contains("## Skills"), "prompt should include Skills section");
        assert!(pr.contains("git-workflow"), "prompt should list skill command");
        assert!(pr.contains("<command>"), "prompt should include command element");
        assert!(pr.contains("available_skills"), "prompt should include available_skills block");
        assert!(pr.contains("load_skill"), "prompt should mention load_skill tool");
    }

    #[test]
    fn system_prompt_no_skills_no_section() {
        let ctx = PromptContext { skills: Arc::from(Vec::<SkillInfo>::new()), ..Default::default() };
        let pr = system_prompt(AgentMode::Agent, None, &no_tools(), ctx);
        assert!(!pr.contains("## Skills"), "prompt should not include Skills section when empty");
        assert!(!pr.contains("<available_skills>"), "prompt should not include available_skills block");
    }

    #[test]
    fn skills_section_omits_user_invocable_only() {
        use sven_runtime::SvenSkillMeta;
        let mut private = make_test_skill("private-skill", "Private tool.");
        private.sven_meta = Some(SvenSkillMeta {
            user_invocable_only: true,
            ..Default::default()
        });
        let public = make_test_skill("public-skill", "Public tool.");
        let skills = vec![private, public];
        let section = build_skills_section(&skills);
        assert!(section.contains("public-skill"), "public skill should be listed");
        assert!(!section.contains("private-skill"), "user_invocable_only skill should be omitted");
    }

    #[test]
    fn skills_section_always_skill_bypasses_budget() {
        use sven_runtime::SvenSkillMeta;
        // Construct an "always" skill and verify it appears even when budget is tight.
        let mut always = make_test_skill("critical", "Always included.");
        always.sven_meta = Some(SvenSkillMeta {
            always: true,
            ..Default::default()
        });
        let skills = vec![always];
        let section = build_skills_section(&skills);
        assert!(section.contains("critical"), "always-skill must appear in section");
    }

    #[test]
    fn skills_section_char_budget_truncates_large_sets() {
        // Create more skills than the budget can hold.
        let skills: Vec<_> = (0..200)
            .map(|i| make_test_skill(
                &format!("skill-{i:03}"),
                &"This skill does task number. ".repeat(20),
            ))
            .collect();
        let section = build_skills_section(&skills);
        assert!(section.len() <= crate::prompts::MAX_SKILLS_PROMPT_CHARS + 500,
            "skills section should be bounded by the char budget (with tolerance for wrapper text)");
        assert!(section.contains("⚠ Skills truncated"), "truncation notice should appear");
    }

    #[test]
    fn build_skills_section_empty_returns_empty_string() {
        let section = build_skills_section(&[]);
        assert!(section.is_empty());
    }

    #[test]
    fn build_skills_section_single_skill_includes_xml_tags() {
        let skills = vec![make_test_skill("my-skill", "Does something.")];
        let section = build_skills_section(&skills);
        assert!(section.contains("<available_skills>"));
        assert!(section.contains("</available_skills>"));
        assert!(section.contains("<command>my-skill</command>"));
        assert!(section.contains("<name>my-skill</name>"));
        assert!(section.contains("<description>Does something.</description>"));
    }
}
