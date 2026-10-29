// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use sven_vocab::AgentMode;
use sven_workspace::{find_workspace_root, AgentInfo, KnowledgeInfo, SkillInfo};

use crate::instruction_index;
use crate::instructions::{self, ListingDisclosure};
pub use crate::prompt_composer::ComposedPrompt;
use crate::prompt_composer::Composer;

/// All optional contextual blocks that can be injected into the system prompt.
#[derive(Debug)]
pub struct PromptContext<'a> {
    /// Absolute path to the project root (from `.git` detection).
    pub project_root: Option<&'a Path>,
    /// Pre-formatted git context (branch, commit, dirty status).
    ///
    /// **Caching note**: this field is *volatile* - it changes on every commit
    /// and with every file edit (dirty count).  When prompt caching is enabled
    /// this content is placed in a *separate, uncached* system block so that
    /// the stable prefix remains cacheable across sessions.
    pub git_context: Option<&'a str>,
    /// Path of the project context file (AGENTS.md / .sven/context.md), when
    /// one exists. Referenced by path in the system prompt, not inlined - see
    /// `context_file_section`'s doc.
    pub project_context_file: Option<&'a Path>,
    /// Pre-formatted CI environment block.
    ///
    /// **Caching note**: like `git_context`, this is volatile between CI runs.
    pub ci_context: Option<&'a str>,
    /// Text appended verbatim after the default Guidelines section.
    pub append: Option<&'a str>,
    /// Discovered skills.  Metadata (name + description) is injected into the
    /// stable system prompt so the model always knows what skills are available.
    /// Held as an `Arc` so a fresh snapshot can be taken from
    /// [`sven_workspace::SharedSkills`] on each turn without cloning the skill data.
    pub skills: Arc<[SkillInfo]>,
    /// Discovered subagents.  Names and descriptions are injected into the
    /// stable system prompt so the model can suggest delegation and the user
    /// can invoke them via slash commands.
    pub agents: Arc<[AgentInfo]>,
    /// Discovered knowledge documents.  A summary section is injected into the
    /// stable system prompt so the model knows the knowledge base exists and
    /// which subsystems it covers.
    pub knowledge: Arc<[KnowledgeInfo]>,
    /// Pre-formatted knowledge drift warning (computed once at session start).
    /// Injected verbatim into the stable system-prompt block.  `None` when all
    /// knowledge documents are current or no `updated:` fields are set.
    pub knowledge_drift_note: Option<&'a str>,
    /// The `## Deferred Tools` section: how tools that are not in the tool
    /// list are loaded, and which exist. `None` when every tool is in the
    /// list. Fixed for the session, so part of the stable prefix.
    pub tool_index: Option<&'a str>,
    /// How much of each catalog entry the Skills, Subagents and Knowledge Base
    /// sections carry.
    pub disclosure: instructions::InstructionDisclosure,
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
            knowledge: Arc::from(Vec::<KnowledgeInfo>::new()),
            knowledge_drift_note: None,
            tool_index: None,
            disclosure: instructions::InstructionDisclosure::default(),
        }
    }
}

impl<'a> PromptContext<'a> {
    /// Return a version of this context with the volatile fields cleared.
    ///
    /// Used to build the *stable* (cacheable) portion of the system prompt.
    /// Skills, agents, knowledge docs, and drift notes are stable within a
    /// session (discovered once at startup) so they are included in the stable
    /// slice.
    pub fn stable_only(&self) -> Self {
        Self {
            project_root: self.project_root,
            git_context: None,
            project_context_file: self.project_context_file,
            ci_context: None,
            append: self.append,
            skills: self.skills.clone(),
            agents: self.agents.clone(),
            knowledge: self.knowledge.clone(),
            knowledge_drift_note: self.knowledge_drift_note,
            tool_index: self.tool_index,
            disclosure: self.disclosure,
        }
    }

    /// Format the volatile fields (git + CI context) as a block suitable for
    /// appending to the system prompt outside the cached region.
    ///
    /// Returns `None` when neither git nor CI context is present.
    pub fn dynamic_block(&self) -> Option<String> {
        let git = self
            .git_context
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        let ci = self
            .ci_context
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
    // Tool-agnostic working method. Guidance for one tool belongs in that
    // tool's own description, which the model only sees while it has the tool.
    pub fn working_method() -> &'static str {
        "- Establish facts from the repository, its tests and command output instead of guessing.\n\
         - Read the code a change touches (its callers, interfaces and tests) before editing it.\n\
         - Reproduce a reported failure before fixing it when you can, and use the reproduction to prove the fix.\n\
         - Make the smallest complete change; follow the existing style and structure; add files only when the task needs them.\n\
         - Verify before finishing: build and run the relevant tests. Never claim a check you did not run.\n\
         - When a command or tool fails, read the error and change the approach; never repeat a failing call unchanged.\n\
         - Issue independent tool calls together in one turn.\n\
         - Keep changes you did not make; never force-push, rewrite published history or skip hooks unless asked.\n\
         - Finish with a short report: what changed, how it was verified, and anything left open."
    }

    pub fn core_tools() -> &'static str {
        "- Locate code with `grep`, then read the relevant ranges with `read_file`.\n\
         - Change existing files with `edit_file` and create new ones with `write_file`.\n\
         - Use `shell` for builds, tests, git and running programs; pass `timeout_secs` for builds and test suites."
    }
}

// ─── Shared budgeted-section helper ──────────────────────────────────────────

/// Fit a list of pre-formatted XML entries within `max_chars`.
///
/// Returns `(fitted_entries, truncation_note)` where `truncation_note` is
/// non-empty when some entries were omitted.  `section_name` is used in the
/// note (e.g. `"Skills"` → `"⚠ Skills truncated: showing N of M."`).
///
/// This is the shared engine behind `build_agents_section` and
/// `build_knowledge_section`, both of which use the same fit-within-budget
/// logic.
fn fit_entries_to_budget<'a>(
    entries: &'a [String],
    total_items: usize,
    max_chars: usize,
    section_name: &str,
) -> (&'a [String], String) {
    let mut used = 0usize;
    let fitted_count = entries
        .iter()
        .take_while(|e| {
            let next = used + e.len();
            if next <= max_chars {
                used = next;
                true
            } else {
                false
            }
        })
        .count();

    let fitted = &entries[..fitted_count];
    let truncation_note = if fitted_count < entries.len() {
        format!(
            "\n⚠ {section_name} truncated: showing {} of {}.",
            fitted_count, total_items
        )
    } else {
        String::new()
    };

    (fitted, truncation_note)
}

// ─── Skills section ───────────────────────────────────────────────────────────

/// Maximum total characters for the `<available_skills>` block in the system
/// prompt.  Binary search is used to fit within this budget when there are many
/// skills.
pub const MAX_SKILLS_PROMPT_CHARS: usize = 30_000;

/// Format the available-skills block for injection into the system prompt.
///
/// Skills with `user_invocable_only: true` are omitted (the `skill` tool
/// still loads one when asked for its exact command).
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
    let (fitted_entries, _) = fit_entries_to_budget(
        &candidate_entries,
        candidate_entries.len(),
        remaining_budget,
        "",
    );
    let fitted_count = fitted_entries.len();

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
         below, call the `skill` tool with `action: \"load\"` and the skill's command as `name` \
         to load the full skill instructions before proceeding. Use the skill `<description>` \
         to decide which skill is absolutely needed for the task. Load at most one skill per \
         task; do not load a skill unless it clearly applies.\
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
            // `is_background` is deliberately not advertised: the task tool
            // always runs a sub-agent to completion, so a <background> hint
            // would promise the model an option it does not have.
            let ro_hint = if a.readonly {
                "\n    <readonly>true</readonly>"
            } else {
                ""
            };
            format!(
                "  <agent>\n    <name>{}</name>\n    <description>{}</description>{}{}\n  </agent>",
                a.name,
                a.description.trim(),
                model_hint,
                ro_hint,
            )
        })
        .collect();

    let (fitted, truncation_note) =
        fit_entries_to_budget(&entries, agents.len(), MAX_AGENTS_PROMPT_CHARS, "Agents");

    if fitted.is_empty() {
        return String::new();
    }

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

/// Format a collab event as a compact, styled line for the chat pane.
///
/// Called by the TUI when rendering `ChatSegment::CollabEvent` entries.
/// Returns a short human-readable summary, e.g.:
/// `"● spawned security-reviewer [reviewer]"`.
pub use sven_session_model::format_collab_event;

/// A collaboration event that can be recorded as a `ChatSegment::CollabEvent`.
///
/// These are display-only entries that track the lifecycle of team operations
/// without adding them to the LLM context.
pub use sven_vocab::CollabEvent;

// ─── Knowledge section ────────────────────────────────────────────────────────

/// Maximum total characters for the `<knowledge_base>` block.
pub const MAX_KNOWLEDGE_PROMPT_CHARS: usize = 8_000;

/// Format the knowledge-base overview block for injection into the system
/// prompt.  Shows subsystem names and their covered file patterns so the model
/// knows which domains have specifications without loading all doc bodies.
///
/// Returns an empty string when `knowledge` is empty.
pub fn build_knowledge_section(knowledge: &[KnowledgeInfo]) -> String {
    if knowledge.is_empty() {
        return String::new();
    }

    let entries: Vec<String> = knowledge
        .iter()
        .map(|k| {
            let files_hint = if k.files.is_empty() {
                String::new()
            } else {
                format!(" ({})", k.files.join(", "))
            };
            let updated_hint = k
                .updated
                .as_deref()
                .map(|d| format!(" - updated {d}"))
                .unwrap_or_default();
            format!(
                "  <doc>\n    <subsystem>{}</subsystem>{}{}\n  </doc>",
                k.subsystem, files_hint, updated_hint
            )
        })
        .collect();

    let (fitted, truncation_note) = fit_entries_to_budget(
        &entries,
        knowledge.len(),
        MAX_KNOWLEDGE_PROMPT_CHARS,
        "Knowledge base",
    );

    if fitted.is_empty() {
        return String::new();
    }

    format!(
        "## Knowledge Base\n\n\
         Project knowledge documents are available for these subsystems.  Use \
         `search_knowledge \"<topic>\"` to find relevant specs before modifying a \
         subsystem, or `list_knowledge` to see all documents with their covered \
         file patterns.{truncation_note}\n\n\
         <knowledge_base>\n{}\n</knowledge_base>",
        fitted.join("\n")
    )
}

/// The items the system prompt lists from the start, and how many it leaves to
/// the per-turn announcement. Borrows the slice when nothing is left out, so a
/// catalog without activation blocks costs nothing and renders as before.
fn unconditional<T: Clone>(items: &[T], keep: fn(&T) -> bool) -> (Cow<'_, [T]>, usize) {
    let dormant = items.iter().filter(|i| !keep(i)).count();
    if dormant == 0 {
        return (Cow::Borrowed(items), 0);
    }
    let listed = items.iter().filter(|i| keep(i)).cloned().collect();
    (Cow::Owned(listed), dormant)
}

/// `section` followed by a line telling the model that `dormant` more items
/// exist and will be announced; the heading alone when `section` is empty.
fn with_dormant_note(section: String, heading: &str, noun: &str, dormant: usize) -> String {
    if dormant == 0 {
        return if section.is_empty() {
            section
        } else {
            format!("\n\n{section}")
        };
    }
    let plural = if dormant == 1 { "is" } else { "are" };
    let note = format!(
        "{dormant} more {noun}{} {plural} announced when they become relevant to the session.",
        if dormant == 1 { "" } else { "s" }
    );
    if section.is_empty() {
        format!("\n\n## {heading}\n\n{note}")
    } else {
        format!("\n\n{section}\n\n{note}")
    }
}

/// The Guidelines block as its two named sections. The first carries the
/// `## Guidelines` heading; each carries the blank line before it.
fn guideline_sections() -> [(&'static str, String); 2] {
    [
        (
            "guidelines.working_method",
            format!(
                "\n\n## Guidelines\n\n### Working Method\n{}",
                guidelines::working_method()
            ),
        ),
        (
            "guidelines.tools",
            format!("\n\n### Tools\n{}", guidelines::core_tools()),
        ),
    ]
}

/// A system prompt that is not the built-in one: `custom` (cut of trailing
/// whitespace when text is appended after it) and `append`, whichever are
/// given. `None` when neither is.
pub(crate) fn compose_override(
    custom: Option<&str>,
    append: Option<&str>,
) -> Option<ComposedPrompt> {
    let mut composer = Composer::default();
    match (custom, append) {
        (None, None) => return None,
        (Some(custom), Some(extra)) => {
            composer.push("custom", custom.trim_end());
            composer.push("append", format!("\n\n{extra}"));
        }
        (Some(custom), None) => {
            composer.push("custom", custom);
        }
        (None, Some(extra)) => {
            composer.push("append", extra);
        }
    }
    Some(composer.finish())
}

/// Build the system prompt for the given agent mode.
///
/// `ctx` carries optional project / CI / git context injected when running
/// in headless mode. This is the text of [`compose_system_prompt`].
pub fn system_prompt(mode: AgentMode, custom: Option<&str>, ctx: PromptContext<'_>) -> String {
    compose_system_prompt(mode, custom, ctx).text
}

/// Build the system prompt for the given agent mode as named sections.
///
/// The sections tile the text (see [`sven_model::manifest`]); their ids are
/// `identity`, `mode`, `project`, `git`, `project_instructions`, `tool_index`,
/// `skills`, `agents`, `knowledge`, `knowledge_drift`, `ci`, the two
/// `guidelines.*` and `append`, or `custom` and `append` for an override. A
/// block with nothing to say has no section.
pub fn compose_system_prompt(
    mode: AgentMode,
    custom: Option<&str>,
    ctx: PromptContext<'_>,
) -> ComposedPrompt {
    if custom.is_some() {
        // Even with a custom prompt, honour append if set.
        return compose_override(custom, ctx.append).expect("a custom prompt is given");
    }

    // Agent identity - fully static so this block is stable across turns
    // and can be cached by Anthropic's prompt-caching layer.
    // Volatile context (git branch, CI env, working directory) is injected
    // separately via system_dynamic_suffix and never touches this block.
    let agent_identity = format!(
        "You are Sven, an autonomous software engineering agent working in a real development \
         environment.\n\n\
         Operating Mode: `{mode}`"
    );

    let mode_instructions = match mode {
        AgentMode::Research => {
            "You are a research assistant.  You may read files, search the codebase, and look up \
             information.  You MUST NOT write, modify, or delete any files. Research mode \
             is non-destructive. Focus on gathering all the information needed in order to \
             satisfy user's request."
        }
        AgentMode::Plan => {
            "You are a planning assistant.  Analyse the request and produce a clear, structured \
             plan with numbered steps.  You may read files to inform the plan, but MUST NOT \
             modify them. If `sven/plan` skill exists then use it. If not then output the plan in Markdown. \
             under `.sven/plans/[unique name].plan.md`. Create directory if it doesn't exist. \
             When a task is ambiguous or you need information to proceed, use the `ask_question` \
             tool to collect structured answers from the user rather than making assumptions or \
             writing a prose question. The `ask_question` tool presents a modal dialog in the TUI; \
             prefer it over free-form text questions whenever the user is interactive.  \
             You can spawn subtasks to explore different parts of the codebase. "

        }
        AgentMode::Agent => {
            "You are a capable coding agent.  You can read and write files, run shell commands, \
             and search the codebase.  Complete the task the user asked for, including every \
             todo you created, before ending your turn."
        }
        AgentMode::Chat => {
            "You are a conversational assistant. You can read files and answer questions. \
             Respond naturally to user messages; the ReactiveAgentMachine drives the chat loop."
        }
        AgentMode::Sdlc => {
            "You are a software development lifecycle agent. You can read and write files, \
             run shell commands, and manage the full development workflow from planning to delivery. \
             The SdlcMachine drives the process through Intake → Discovery → Planning → Execution \
             → Verification → Delivery phases."
        }
    };

    let project_section = if let Some(root) = ctx.project_root {
        let workspace_root = find_workspace_root(root);
        let workspace_line = if workspace_root != root {
            format!(
                "\nWorkspace root: `{}` (contains shared tooling above the git repository)\n\
                 - When the user provides relative paths, resolve them relative to the \
                   workspace root.",
                workspace_root.display()
            )
        } else {
            String::from(
                "\nWorkspace root: same as project root.\n\
                 - When the user provides relative paths, resolve them relative to the \
                   project root.",
            )
        };
        format!(
            "\n\n## Project Context\n\
             Project root: `{project_root}`\
             {workspace_line}\n\
             - Use absolute paths for all file read/write operations.",
            project_root = root.display(),
        )
    } else {
        // No git root does not mean no location. Said nothing, the model has
        // to guess where it is running, and it guesses: asked to list the
        // current directory it produced `/current_directory` once and `/tmp`
        // another time. A working directory is cheap to state and is the one
        // fact every path argument depends on.
        match std::env::current_dir() {
            Ok(cwd) => format!(
                "\n\n## Project Context\n\
                 Working directory: `{}`\n\
                 - Not a git repository, so there is no project root above it.\n\
                 - Resolve relative paths against this directory.",
                cwd.display()
            ),
            Err(_) => String::new(),
        }
    };

    let git_section = if let Some(git) = ctx.git_context {
        format!("\n\n{git}")
    } else {
        String::new()
    };

    // Project context file (AGENTS.md / .sven/context.md) - referenced by
    // path, never inlined. Every turn used to pay for the file's full content
    // whether or not the task ever touched it; on a large file that alone can
    // be a meaningful fraction of the context window, and combined with other
    // long input (a big image, a long conversation) can push a request well
    // past what's actually needed. The model reads it itself with its normal
    // file tool on the turns where it's actually relevant.
    let context_file_section = if let Some(path) = ctx.project_context_file {
        format!(
            "\n\n## Project Instructions\n\nThis project has instructions at `{}`. \
             Read it if it's relevant to your task.",
            path.display()
        )
    } else {
        String::new()
    };

    let ci_section = if let Some(ci) = ctx.ci_context {
        format!("\n\n{ci}")
    } else {
        String::new()
    };

    // Deferred tools - stable for the session, ahead of the skills so the
    // two lists a model chooses from sit together.
    let tool_index_section = match ctx.tool_index {
        Some(index) if !index.trim().is_empty() => format!("\n\n{}", index.trim_end()),
        _ => String::new(),
    };

    // Skills - stable, injected after project instructions and before CI/git.
    // Instructions with an activation condition are announced per turn once it
    // holds (see `crate::instructions`), so the stable prefix never depends on
    // what a session touches.
    let skills_section = {
        let (listed, dormant) = unconditional(&ctx.skills, instructions::skill_is_unconditional);
        let s = match ctx.disclosure.skills {
            ListingDisclosure::Full => build_skills_section(&listed),
            ListingDisclosure::Deferred => instruction_index::build_skills_index(&listed),
        };
        with_dormant_note(s, "Skills", "skill", dormant)
    };

    // Agents - stable, injected after skills.
    let agents_section = {
        let (listed, dormant) = unconditional(&ctx.agents, instructions::agent_is_unconditional);
        let s = match ctx.disclosure.subagents {
            ListingDisclosure::Full => build_agents_section(&listed),
            ListingDisclosure::Deferred => instruction_index::build_agents_index(&listed),
        };
        with_dormant_note(s, "Subagents", "subagent", dormant)
    };

    // Knowledge base overview - stable, injected after agents.
    let knowledge_section = {
        let (listed, dormant) =
            unconditional(&ctx.knowledge, instructions::knowledge_is_unconditional);
        let s = match ctx.disclosure.knowledge {
            ListingDisclosure::Full => build_knowledge_section(&listed),
            ListingDisclosure::Deferred => instruction_index::build_knowledge_index(&listed),
        };
        with_dormant_note(s, "Knowledge Base", "knowledge document", dormant)
    };

    // Knowledge drift warning - stable (computed once at session start).
    let knowledge_drift_section = if let Some(note) = ctx.knowledge_drift_note {
        format!("\n\n{note}")
    } else {
        String::new()
    };

    let append_section = ctx.append.map(|extra| format!("\n\n{extra}"));

    let mut composer = Composer::default();
    composer
        .push("identity", agent_identity)
        .push("mode", format!("\n\n{mode_instructions}"))
        .push("project", project_section)
        .push("git", git_section)
        .push("project_instructions", context_file_section)
        .push("tool_index", tool_index_section)
        .push("skills", skills_section)
        .push("agents", agents_section)
        .push("knowledge", knowledge_section)
        .push("knowledge_drift", knowledge_drift_section)
        .push("ci", ci_section);
    for (id, section) in guideline_sections() {
        composer.push(id, section);
    }
    composer.push("append", append_section.unwrap_or_default());
    composer.finish()
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use sven_vocab::AgentMode;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn empty() -> PromptContext<'static> {
        PromptContext::default()
    }

    #[test]
    fn custom_prompt_is_returned_verbatim() {
        let prompt = system_prompt(AgentMode::Agent, Some("Custom instructions here."), empty());
        assert_eq!(prompt, "Custom instructions here.");
    }

    #[test]
    fn custom_prompt_with_append() {
        let ctx = PromptContext {
            append: Some("Extra rule."),
            ..Default::default()
        };
        let prompt = system_prompt(AgentMode::Agent, Some("Base."), ctx);
        assert!(prompt.contains("Base."));
        assert!(prompt.contains("Extra rule."));
    }

    #[test]
    fn research_mode_mentions_read_only() {
        let pr = system_prompt(AgentMode::Research, None, empty());
        assert!(
            pr.contains("read-only") || pr.contains("MUST NOT write"),
            "Research mode should forbid writes"
        );
    }

    #[test]
    fn plan_mode_mentions_structured_plan() {
        let pr = system_prompt(AgentMode::Plan, None, empty());
        assert!(
            pr.to_lowercase().contains("plan"),
            "Plan mode prompt should mention 'plan'"
        );
    }

    #[test]
    fn agent_mode_mentions_write_capability() {
        let pr = system_prompt(AgentMode::Agent, None, empty());
        assert!(
            pr.contains("write files") || pr.contains("read and write"),
            "Agent mode should mention write capability"
        );
    }

    #[test]
    fn all_modes_name_sven() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, empty());
            assert!(
                pr.contains("Sven"),
                "prompt should identify the agent as Sven"
            );
        }
    }

    #[test]
    fn all_modes_include_mode_name_in_prompt() {
        for (mode, expected) in [
            (AgentMode::Research, "research"),
            (AgentMode::Plan, "plan"),
            (AgentMode::Agent, "agent"),
        ] {
            let pr = system_prompt(mode, None, empty());
            assert!(
                pr.contains(expected),
                "prompt for {mode} should contain the mode name"
            );
        }
    }

    #[test]
    fn all_modes_include_guidelines_section() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, empty());
            assert!(
                pr.contains("Guidelines"),
                "prompt should contain a Guidelines section"
            );
        }
    }

    #[test]
    fn project_root_appears_in_prompt() {
        let root = p("/project/my-project");
        let ctx = PromptContext {
            project_root: Some(&root),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(
            pr.contains("/project/my-project"),
            "project root should appear in prompt"
        );
        assert!(
            pr.contains("Project Context"),
            "prompt should have Project Context section"
        );
    }

    /// No git root still tells the model where it is.
    ///
    /// Saying nothing was the old behaviour and it is the bug: a model with no
    /// location has to guess one for every path argument, and it does - asked
    /// to list the current directory it produced `/current_directory` once and
    /// `/tmp` another time, then reported confidently on whatever it found
    /// there.
    #[test]
    fn no_project_root_still_names_the_working_directory() {
        let pr = system_prompt(AgentMode::Agent, None, empty());
        let cwd = std::env::current_dir().unwrap();
        assert!(pr.contains("Working directory"), "{pr}");
        assert!(pr.contains(&cwd.display().to_string()), "{pr}");
        // The git-rooted phrasing must not appear when there is no git root.
        assert!(!pr.contains("Project root:"), "{pr}");
    }

    #[test]
    fn ci_context_is_appended() {
        let ci = "## CI Environment\nRunning in: GitHub Actions\nBranch: main";
        let ctx = PromptContext {
            ci_context: Some(ci),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(pr.contains("GitHub Actions"));
        assert!(pr.contains("Branch: main"));
    }

    #[test]
    fn git_context_appears_in_prompt() {
        let git = "## Git Context\nBranch: main\nCommit: abc1234";
        let ctx = PromptContext {
            git_context: Some(git),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(pr.contains("Git Context"));
        assert!(pr.contains("abc1234"));
    }

    #[test]
    fn project_context_file_is_referenced_by_path_not_inlined() {
        // The system prompt must never inline the file's content on every
        // turn (that content could be arbitrarily large and irrelevant to
        // most tasks) -- only reference its path so the model can read it
        // itself when relevant.
        let path = Path::new("AGENTS.md");
        let ctx = PromptContext {
            project_context_file: Some(path),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(pr.contains("Project Instructions"));
        assert!(pr.contains("AGENTS.md"));
    }

    #[test]
    fn append_section_is_added_after_guidelines() {
        let ctx = PromptContext {
            append: Some("Custom rule: never delete files."),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        let guidelines_pos = pr.find("Guidelines").unwrap();
        let append_pos = pr.find("Custom rule").unwrap();
        assert!(
            append_pos > guidelines_pos,
            "append should come after Guidelines"
        );
    }

    #[test]
    fn the_core_prompt_instructs_only_tools_every_session_has() {
        // A tool that is deferred or not configured is absent from the tool
        // list; instructing it costs tokens and invites calls that fail.
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let pr = system_prompt(mode, None, empty());
            for absent in [
                "`gdb`",
                "`memory`",
                "`context`",
                "`task`",
                "memory(",
                "context(",
                "task(",
            ] {
                assert!(!pr.contains(absent), "mode {mode} names {absent}");
            }
        }
    }

    #[test]
    fn the_core_prompt_asks_for_evidence_and_verification() {
        let pr = system_prompt(AgentMode::Agent, None, empty());
        assert!(pr.contains("instead of guessing"));
        assert!(pr.contains("Verify before finishing"));
        assert!(pr.contains("Never claim a check you did not run"));
        assert!(pr.contains("never force-push"));
    }

    // ── Skills section tests ──────────────────────────────────────────────────

    fn make_test_skill(command: &str, description: &str) -> sven_workspace::SkillInfo {
        use std::path::PathBuf;
        let name = command.rsplit('/').next().unwrap_or(command).to_string();
        sven_workspace::SkillInfo {
            command: command.to_string(),
            name,
            description: description.to_string(),
            version: None,
            skill_md_path: PathBuf::from(format!("skills/{command}/SKILL.md")),
            skill_dir: PathBuf::from(format!("skills/{command}")),
            content: format!("## {command} content"),
            sven_meta: None,
        }
    }

    #[test]
    fn system_prompt_includes_skills_section_when_skills_provided() {
        let skills = vec![make_test_skill(
            "git-workflow",
            "Use when the user asks about git.",
        )];
        let ctx = PromptContext {
            skills: Arc::from(skills.into_boxed_slice()),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(
            pr.contains("## Skills"),
            "prompt should include Skills section"
        );
        assert!(
            pr.contains("git-workflow"),
            "prompt should list skill command"
        );
        assert!(
            pr.contains("<command>"),
            "prompt should include command element"
        );
        assert!(
            pr.contains("available_skills"),
            "prompt should include available_skills block"
        );
        // The tool the prompt sends the model to must be the one registered
        // (`SkillTool`, named `skill`, loading with `action: "load"`).
        assert!(
            pr.contains("`skill` tool with `action: \"load\"`"),
            "prompt should name the skill tool and its load action: {pr}"
        );
        assert!(!pr.contains("load_skill"), "no such tool: {pr}");
        assert!(
            pr.contains("for the task. Load at most one skill"),
            "one paragraph, no source indentation: {pr}"
        );
    }

    #[test]
    fn system_prompt_no_skills_no_section() {
        let ctx = PromptContext {
            skills: Arc::from(Vec::<SkillInfo>::new()),
            ..Default::default()
        };
        let pr = system_prompt(AgentMode::Agent, None, ctx);
        assert!(
            !pr.contains("## Skills"),
            "prompt should not include Skills section when empty"
        );
        assert!(
            !pr.contains("<available_skills>"),
            "prompt should not include available_skills block"
        );
    }

    #[test]
    fn skills_section_omits_user_invocable_only() {
        use sven_workspace::SvenSkillMeta;
        let mut private = make_test_skill("private-skill", "Private tool.");
        private.sven_meta = Some(SvenSkillMeta {
            user_invocable_only: true,
            ..Default::default()
        });
        let public = make_test_skill("public-skill", "Public tool.");
        let skills = vec![private, public];
        let section = build_skills_section(&skills);
        assert!(
            section.contains("public-skill"),
            "public skill should be listed"
        );
        assert!(
            !section.contains("private-skill"),
            "user_invocable_only skill should be omitted"
        );
    }

    #[test]
    fn skills_section_always_skill_bypasses_budget() {
        use sven_workspace::SvenSkillMeta;
        // Construct an "always" skill and verify it appears even when budget is tight.
        let mut always = make_test_skill("critical", "Always included.");
        always.sven_meta = Some(SvenSkillMeta {
            always: true,
            ..Default::default()
        });
        let skills = vec![always];
        let section = build_skills_section(&skills);
        assert!(
            section.contains("critical"),
            "always-skill must appear in section"
        );
    }

    #[test]
    fn skills_section_char_budget_truncates_large_sets() {
        // Create more skills than the budget can hold.
        let skills: Vec<_> = (0..200)
            .map(|i| {
                make_test_skill(
                    &format!("skill-{i:03}"),
                    &"This skill does task number. ".repeat(20),
                )
            })
            .collect();
        let section = build_skills_section(&skills);
        assert!(
            section.len() <= crate::prompts::MAX_SKILLS_PROMPT_CHARS + 500,
            "skills section should be bounded by the char budget (with tolerance for wrapper text)"
        );
        assert!(
            section.contains("⚠ Skills truncated"),
            "truncation notice should appear"
        );
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
