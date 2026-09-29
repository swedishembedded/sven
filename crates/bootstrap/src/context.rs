// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runtime context types for agent construction.
//!
//! [`RuntimeContext`] holds environment-detected information (project root,
//! git state, CI environment) that is not part of the config file schema.
//!
//! [`ToolSetProfile`] selects which tools to register, and carries the
//! shared state needed by stateful tools (todos, mode lock, GDB state).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_tool_api::events::TodoItem;
use sven_tools_agent::QuestionRequest;
use sven_tools_fs::OutputBufferStore;
use sven_turn::AgentRuntimeContext;
use sven_workspace::{CiContext, GitContext, SharedAgents, SharedKnowledge, SharedSkills};

// ─── RuntimeContext ───────────────────────────────────────────────────────────

/// Environment-detected context for an agent session.
///
/// This is separate from [`sven_config::AgentConfig`] (which holds only
/// config-file fields) so that the two concerns - "what the user configured"
/// and "what we found at runtime" - stay cleanly separated.
#[derive(Default, Clone)]
pub struct RuntimeContext {
    /// Absolute path to the project root (detected from `.git` walk-up).
    pub project_root: Option<PathBuf>,
    /// Live git metadata (branch, commit, dirty state).
    pub git_context: Option<GitContext>,
    /// CI environment metadata.
    pub ci_context: Option<CiContext>,
    /// Path of `.sven/context.md`, `AGENTS.md`, or `CLAUDE.md`, when one
    /// exists. Referenced by path in the system prompt, not inlined — see
    /// `sven_core::prompts`.
    pub project_context_file: Option<PathBuf>,
    /// Text appended after the default system prompt Guidelines section.
    pub append_system_prompt: Option<String>,
    /// Full system prompt override (from `--system-prompt-file`).
    pub system_prompt_override: Option<String>,
    /// Suppress Sven's built-in identity/guidelines/context prompt
    /// (from `--no-system`). See [`sven_turn::AgentRuntimeContext::build_system_message`].
    pub no_system: bool,
    /// Suppress tool availability entirely (from `--no-tools`): no tool
    /// schemas are sent to the model and any tool call is refused.
    pub no_tools: bool,
    /// Skills discovered from the standard search hierarchy.
    ///
    /// Using [`SharedSkills`] allows the TUI to share the same instance and
    /// trigger a live refresh (via `/refresh`) without restarting the agent.
    pub skills: SharedSkills,
    /// Subagents discovered from the standard search hierarchy.
    pub agents: SharedAgents,
    /// Knowledge documents discovered from `.sven/knowledge/`.
    pub knowledge: SharedKnowledge,
    /// Pre-formatted knowledge drift warning (computed once at startup).
    /// `None` when all documents are current or none have `updated:` fields.
    pub knowledge_drift_note: Option<String>,
}

impl RuntimeContext {
    /// Create with auto-detected project, git, CI context, skills, and knowledge.
    pub fn auto_detect() -> Self {
        Self::auto_detect_at(sven_workspace::find_project_root().ok())
    }

    /// Create with detected git/CI/skills/agents/knowledge context, using an
    /// already-resolved `project_root` instead of walking up from the current
    /// directory.
    ///
    /// This is the shared detection logic `auto_detect()` builds on; callers
    /// that already have a resolved project root (headless runners passed
    /// `--cwd`, a rebuilt session reusing the original root, ...) should call
    /// this directly and override individual fields with struct-update syntax
    /// (`RuntimeContext { no_system: true, ..RuntimeContext::auto_detect_at(root) }`)
    /// rather than re-deriving skills/agents/knowledge/drift by hand -- a
    /// hand-written copy previously dropped the knowledge-drift check by
    /// hardcoding `knowledge_drift_note: None`.
    pub fn auto_detect_at(project_root: Option<PathBuf>) -> Self {
        Self::auto_detect_with(
            project_root.clone(),
            SharedSkills::new(sven_workspace::discover_skills(project_root.as_deref())),
            SharedAgents::new(sven_workspace::discover_agents(project_root.as_deref())),
        )
    }

    /// Create with detected git/CI/knowledge context at `project_root`,
    /// reusing already-discovered `skills`/`agents` instead of re-walking the
    /// skill/agent search hierarchy.
    ///
    /// For a long-lived interactive session (TUI) that discovers skills and
    /// agents once at startup and then spawns a kernel session task, this is
    /// the difference between one filesystem walk and two: the task no
    /// longer needs to call [`Self::auto_detect`] (which would discover its
    /// own, separate copies) and can instead pass through what the caller
    /// already has.
    pub fn auto_detect_with(
        project_root: Option<PathBuf>,
        skills: SharedSkills,
        agents: SharedAgents,
    ) -> Self {
        let git_context = project_root
            .as_ref()
            .map(|r| sven_workspace::collect_git_context(r));
        let ci_context = Some(sven_workspace::detect_ci_context());
        let project_context_file = project_root
            .as_ref()
            .and_then(|r| sven_workspace::find_project_context_file(r));

        // Discover knowledge docs and check for drift against recent git commits.
        let knowledge_items = sven_workspace::discover_knowledge(project_root.as_deref());
        let knowledge_drift_note = project_root
            .as_ref()
            .map(|r| sven_workspace::check_knowledge_drift(r, &knowledge_items))
            .and_then(|warnings| sven_workspace::format_drift_warnings(&warnings));
        let knowledge = SharedKnowledge::new(knowledge_items);

        Self {
            project_root,
            git_context,
            ci_context,
            project_context_file,
            append_system_prompt: None,
            system_prompt_override: None,
            no_system: false,
            no_tools: false,
            skills,
            agents,
            knowledge,
            knowledge_drift_note,
        }
    }

    /// Create an empty context (no project/git/CI detection).
    pub fn empty() -> Self {
        Self {
            knowledge: SharedKnowledge::empty(),
            ..Default::default()
        }
    }

    /// Convert this [`RuntimeContext`] into an [`AgentRuntimeContext`] suitable
    /// for seeding the kernel runtime built by [`RuntimeBuilder`].
    ///
    /// The resulting context carries project/git/CI notes, skills, agents, and
    /// knowledge but leaves `append_system_prompt`, `system_prompt_override`,
    /// `no_system`, `no_tools`, and `prior_messages` at their defaults -
    /// callers that need to inject additional prompt text or pre-loaded
    /// messages should mutate the returned struct before use (see
    /// `RuntimeBuilder::build`, which does exactly that for these fields).
    pub fn to_agent_runtime(&self) -> AgentRuntimeContext {
        AgentRuntimeContext {
            project_root: self.project_root.clone(),
            git_context_note: self
                .git_context
                .as_ref()
                .and_then(|g| g.to_prompt_section()),
            ci_context_note: self.ci_context.as_ref().and_then(|c| c.to_prompt_section()),
            project_context_file: self.project_context_file.clone(),
            skills: self.skills.clone(),
            agents: self.agents.clone(),
            knowledge: self.knowledge.clone(),
            knowledge_drift_note: self.knowledge_drift_note.clone(),
            ..AgentRuntimeContext::default()
        }
    }
}

// ─── BuiltinTools ─────────────────────────────────────────────────────────────

/// Which built-in tools a session registers. Tools a caller registers itself
/// (`RuntimeBuilder::with_extra_tools`) are added on top in every case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuiltinTools {
    /// Chosen from the mode, the project and the process
    /// ([`ToolSetProfile::for_session`]), with the configured MCP servers:
    /// what the sven application runs with.
    #[default]
    Detect,
    /// The coding preset: files, search, shell, todo and `ask_question`.
    Coding,
    /// The read-only research preset.
    Research,
    /// No built-in tools and no MCP servers.
    None,
}

// ─── Questions ────────────────────────────────────────────────────────────────

/// Where the `ask_question` tool takes what the model asks.
pub enum Questions {
    /// To a surface that answers while the run waits (the TUI).
    Answered(mpsc::Sender<QuestionRequest>),
    /// Parked: the run stops on the question and resumes when an answer is
    /// posted for it, however much later that is.
    Parked,
    /// Not offered: the session has no `ask_question` tool.
    Unavailable,
}

impl Questions {
    /// A surface's question channel, or no `ask_question` tool without one:
    /// what a session the application assembles runs with.
    fn answered_or_unavailable(tx: Option<mpsc::Sender<QuestionRequest>>) -> Self {
        tx.map_or(Self::Unavailable, Self::Answered)
    }
}

// ─── ToolSetProfile ───────────────────────────────────────────────────────────

/// Session-locked profile that selects the tool set for an entire session.
///
/// Profiles are detected once at session start (via `detect_profile`) and
/// never change mid-session, which keeps the Anthropic prefix-cache for the
/// tools array stable across all turns.
///
pub enum ToolSetProfile {
    /// Full tool set (TUI and headless/CI, with GDB and context tools).
    ///
    /// Use when the project has GDB configuration or large-content analysis
    /// is expected.
    Full {
        questions: Questions,
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    },

    /// Coding profile (default - no GDB, no context).
    ///
    /// For typical software engineering sessions without embedded debugging
    /// or large-file analysis. Leaner tools array caches more efficiently.
    Coding {
        questions: Questions,
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    },

    /// Research profile (read-only, no write tools).
    ///
    /// For exploration sessions where the agent should not modify files.
    /// No edit_file, write, shell (modifying commands), or task.
    Research {
        questions: Questions,
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },

    /// Sub-agent tool set (Coding minus ask_question, minus task).
    ///
    /// Prevents unbounded nesting. Sub-agents should not spawn further
    /// sub-agents or interrupt the user with questions.
    SubAgent {
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    },
}

impl ToolSetProfile {
    /// Auto-detect the appropriate profile from the runtime context and agent mode.
    ///
    /// Detection heuristics (evaluated in priority order):
    /// 1. If `is_sub_agent` → `SubAgent`
    /// 2. If agent mode is Research → `Research`
    /// 3. If project has GDB config (`.gdbinit`, `openocd.cfg`, `debugging/`) → `Full`
    /// 4. Default → `Coding`
    pub fn detect(
        is_sub_agent: bool,
        mode: sven_config::AgentMode,
        project_root: Option<&std::path::Path>,
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    ) -> Self {
        if is_sub_agent {
            return ToolSetProfile::SubAgent {
                todos,
                buffer_store,
            };
        }

        let questions = Questions::answered_or_unavailable(question_tx);
        if mode == sven_config::AgentMode::Research {
            return ToolSetProfile::Research { questions, todos };
        }

        if has_gdb_config(project_root) {
            return ToolSetProfile::Full {
                questions,
                todos,
                buffer_store,
            };
        }

        ToolSetProfile::Coding {
            questions,
            todos,
            buffer_store,
        }
    }

    /// The profile for a live session, from the runtime context and the
    /// environment.
    ///
    /// Wraps [`Self::detect`] with the one fact a caller cannot supply
    /// cleanly: whether this process IS a sub-agent. That is read from the
    /// depth variable `TaskTool` sets on the child it spawns, so the two
    /// halves cannot disagree about what a sub-agent is.
    pub fn for_session(
        agent_mode: sven_config::AgentMode,
        project_root: Option<&std::path::Path>,
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    ) -> Self {
        let profile = Self::detect(
            std::env::var(crate::task_tool::SUBAGENT_DEPTH_ENV).is_ok(),
            agent_mode,
            project_root,
            question_tx,
            todos,
            buffer_store,
        );
        tracing::debug!(
            profile = profile.name(),
            "resolved tool set for this session"
        );
        profile
    }

    /// The profile `selection` names, or `None` when it names no built-in
    /// tools at all.
    pub fn for_selection(
        selection: BuiltinTools,
        agent_mode: sven_config::AgentMode,
        project_root: Option<&std::path::Path>,
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
        buffer_store: Arc<Mutex<OutputBufferStore>>,
    ) -> Option<Self> {
        match selection {
            BuiltinTools::Detect => Some(Self::for_session(
                agent_mode,
                project_root,
                question_tx,
                todos,
                buffer_store,
            )),
            // An explicit preset is an embedding host's choice; with no
            // surface to answer a question, asking parks the run.
            BuiltinTools::Coding => Some(ToolSetProfile::Coding {
                questions: question_tx.map_or(Questions::Parked, Questions::Answered),
                todos,
                buffer_store,
            }),
            BuiltinTools::Research => Some(ToolSetProfile::Research {
                questions: question_tx.map_or(Questions::Parked, Questions::Answered),
                todos,
            }),
            BuiltinTools::None => None,
        }
    }

    /// Returns a short name for the profile (for logging/display).
    pub fn name(&self) -> &'static str {
        match self {
            ToolSetProfile::Full { .. } => "full",
            ToolSetProfile::Coding { .. } => "coding",
            ToolSetProfile::Research { .. } => "research",
            ToolSetProfile::SubAgent { .. } => "subagent",
        }
    }
}

/// Returns `true` when the project root contains GDB configuration files
/// indicating that embedded debugging tools are needed.
pub(crate) fn has_gdb_config(project_root: Option<&std::path::Path>) -> bool {
    let root = match project_root {
        Some(r) => r,
        None => return false,
    };

    // Common GDB config files / directories
    for indicator in &[
        ".gdbinit",
        "openocd.cfg",
        "openocd_board.cfg",
        "pyocd.yaml",
        "pyocd.yml",
        "JLinkSettings.ini",
        "debugging",
    ] {
        if root.join(indicator).exists() {
            return true;
        }
    }

    // Also check for .vscode/launch.json or debugging/launch.json with GDB config
    let launch_paths = [
        root.join(".vscode").join("launch.json"),
        root.join("debugging").join("launch.json"),
    ];
    for launch_path in &launch_paths {
        if let Ok(content) = std::fs::read_to_string(launch_path) {
            if content.contains("gdb") || content.contains("GDB") || content.contains("JLink") {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use sven_config::AgentMode;
    use sven_tool_api::events::TodoItem;
    use sven_tools_fs::OutputBufferStore;
    use sven_workspace::{SharedAgents, SharedSkills};

    use super::{has_gdb_config, RuntimeContext, ToolSetProfile};

    fn todos() -> Arc<Mutex<Vec<TodoItem>>> {
        Arc::new(Mutex::new(vec![]))
    }

    fn buffer_store() -> Arc<Mutex<OutputBufferStore>> {
        Arc::new(Mutex::new(OutputBufferStore::new()))
    }

    // ── auto_detect_with ─────────────────────────────────────────────────────

    fn skill(command: &str) -> sven_workspace::SkillInfo {
        sven_workspace::SkillInfo {
            command: command.to_string(),
            name: command.to_string(),
            description: String::new(),
            version: None,
            skill_md_path: std::path::PathBuf::new(),
            skill_dir: std::path::PathBuf::new(),
            content: String::new(),
            sven_meta: None,
        }
    }

    // Pins the fix for a regression where a caller with its own
    // already-discovered skills/agents (the TUI, which discovers them once at
    // startup) got a *second*, separate discovery pass baked into its kernel
    // session's RuntimeContext via a bare `RuntimeContext::auto_detect()` call
    // that ignored what it was handed. `auto_detect_with` must use exactly the
    // given skills/agents, not re-derive them from `project_root` -- proven
    // here by pointing it at an empty project root (which would discover zero
    // skills on disk) while passing in a non-empty pre-discovered list.
    #[test]
    fn auto_detect_with_reuses_given_skills_and_agents_instead_of_rediscovering() {
        let empty_root = tempfile::tempdir().unwrap();
        let skills = SharedSkills::new(vec![skill("preloaded")]);
        let agents = SharedAgents::empty();

        let ctx =
            RuntimeContext::auto_detect_with(Some(empty_root.path().to_path_buf()), skills, agents);

        let got = ctx.skills.get();
        assert_eq!(
            got.len(),
            1,
            "should carry the caller's pre-discovered skill"
        );
        assert_eq!(got[0].command, "preloaded");
    }

    // Pins the fix for a second regression: two headless-runner call sites
    // hand-wrote a `RuntimeContext { .. }` literal that hardcoded
    // `knowledge_drift_note: None` and never called `discover_knowledge` at
    // all, so `RuntimeRunner` runs never saw knowledge docs (or warned about
    // stale ones) while `CiRunner` runs did. Both now build on
    // `auto_detect_at`, which always runs real discovery -- proven here by
    // planting an actual `.sven/knowledge/*.md` file and asserting it comes
    // back, which a hardcoded-empty literal could not produce.
    #[test]
    fn auto_detect_at_actually_discovers_knowledge_docs() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(".sven").join("knowledge");
        std::fs::create_dir_all(&knowledge_dir).unwrap();
        std::fs::write(
            knowledge_dir.join("notes.md"),
            "---\nsubsystem: Test Subsystem\n---\n\n## Notes\n\nSome content.",
        )
        .unwrap();

        let ctx = RuntimeContext::auto_detect_at(Some(dir.path().to_path_buf()));

        assert_eq!(
            ctx.knowledge.get().len(),
            1,
            "auto_detect_at should have run real discovery, not a hardcoded empty list"
        );
    }

    // ── has_gdb_config ────────────────────────────────────────────────────────

    #[test]
    fn has_gdb_config_returns_false_for_none() {
        assert!(!has_gdb_config(None));
    }

    #[test]
    fn has_gdb_config_returns_false_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_gdb_config(Some(dir.path())));
    }

    #[test]
    fn has_gdb_config_detects_gdbinit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gdbinit"), "").unwrap();
        assert!(has_gdb_config(Some(dir.path())));
    }

    #[test]
    fn has_gdb_config_detects_openocd_cfg() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("openocd.cfg"), "").unwrap();
        assert!(has_gdb_config(Some(dir.path())));
    }

    #[test]
    fn has_gdb_config_detects_vscode_launch_with_gdb() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode/launch.json"),
            r#"{"configurations": [{"type": "gdb"}]}"#,
        )
        .unwrap();
        assert!(has_gdb_config(Some(dir.path())));
    }

    // ── ToolSetProfile::detect ────────────────────────────────────────────────

    #[test]
    fn detect_sub_agent_returns_subagent_profile() {
        let profile =
            ToolSetProfile::detect(true, AgentMode::Agent, None, None, todos(), buffer_store());
        assert_eq!(profile.name(), "subagent");
    }

    #[test]
    fn detect_research_mode_returns_research_profile() {
        let profile = ToolSetProfile::detect(
            false,
            AgentMode::Research,
            None,
            None,
            todos(),
            buffer_store(),
        );
        assert_eq!(profile.name(), "research");
    }

    #[test]
    fn detect_with_gdb_config_returns_full_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gdbinit"), "").unwrap();
        let profile = ToolSetProfile::detect(
            false,
            AgentMode::Agent,
            Some(dir.path()),
            None,
            todos(),
            buffer_store(),
        );
        assert_eq!(profile.name(), "full");
    }

    #[test]
    fn detect_default_returns_coding_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profile = ToolSetProfile::detect(
            false,
            AgentMode::Agent,
            Some(dir.path()),
            None,
            todos(),
            buffer_store(),
        );
        assert_eq!(profile.name(), "coding");
    }

    #[test]
    fn detect_sub_agent_takes_priority_over_research_mode() {
        let profile = ToolSetProfile::detect(
            true,
            AgentMode::Research,
            None,
            None,
            todos(),
            buffer_store(),
        );
        assert_eq!(
            profile.name(),
            "subagent",
            "sub-agent flag must take priority"
        );
    }
}
