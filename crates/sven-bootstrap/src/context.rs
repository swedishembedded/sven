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

use sven_core::AgentRuntimeContext;
use sven_runtime::{CiContext, GitContext, SharedAgents, SharedKnowledge, SharedSkills};
use sven_tools::{events::TodoItem, QuestionRequest};

// ─── RuntimeContext ───────────────────────────────────────────────────────────

/// Environment-detected context for an agent session.
///
/// This is separate from [`sven_config::AgentConfig`] (which holds only
/// config-file fields) so that the two concerns — "what the user configured"
/// and "what we found at runtime" — stay cleanly separated.
#[derive(Default)]
pub struct RuntimeContext {
    /// Absolute path to the project root (detected from `.git` walk-up).
    pub project_root: Option<PathBuf>,
    /// Live git metadata (branch, commit, dirty state).
    pub git_context: Option<GitContext>,
    /// CI environment metadata.
    pub ci_context: Option<CiContext>,
    /// Contents of `.sven/context.md`, `AGENTS.md`, or `CLAUDE.md`.
    pub project_context_file: Option<String>,
    /// Text appended after the default system prompt Guidelines section.
    pub append_system_prompt: Option<String>,
    /// Full system prompt override (from `--system-prompt-file`).
    pub system_prompt_override: Option<String>,
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
        let project_root = sven_runtime::find_project_root().ok();
        let git_context = project_root
            .as_ref()
            .map(|r| sven_runtime::collect_git_context(r));
        let ci_context = Some(sven_runtime::detect_ci_context());
        let project_context_file = project_root
            .as_ref()
            .and_then(|r| sven_runtime::load_project_context_file(r));
        let skills = SharedSkills::new(sven_runtime::discover_skills(project_root.as_deref()));
        let agents = SharedAgents::new(sven_runtime::discover_agents(project_root.as_deref()));

        // Discover knowledge docs and check for drift against recent git commits.
        let knowledge_items = sven_runtime::discover_knowledge(project_root.as_deref());
        let knowledge_drift_note = project_root
            .as_ref()
            .map(|r| sven_runtime::check_knowledge_drift(r, &knowledge_items))
            .and_then(|warnings| sven_runtime::format_drift_warnings(&warnings));
        let knowledge = SharedKnowledge::new(knowledge_items);

        Self {
            project_root,
            git_context,
            ci_context,
            project_context_file,
            append_system_prompt: None,
            system_prompt_override: None,
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
    /// for passing to [`sven_core::Agent::new`].
    ///
    /// The resulting context carries project/git/CI notes, skills, agents, and
    /// knowledge but leaves `append_system_prompt` and `prior_messages` at
    /// their defaults — callers that need to inject additional prompt text or
    /// pre-loaded messages should mutate the returned struct before use.
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

// ─── ToolSetProfile ───────────────────────────────────────────────────────────

/// Session-locked profile that selects the tool set for an entire session.
///
/// Profiles are detected once at session start (via `detect_profile`) and
/// never change mid-session, which keeps the Anthropic prefix-cache for the
/// tools array stable across all turns.
///
/// `question_tx` is `Some` when ask_question routes to the TUI; `None` for
/// headless/CI/sub-agent contexts where no UI is attached.
pub enum ToolSetProfile {
    /// Full tool set (TUI and headless/CI, with GDB and context tools).
    ///
    /// Use when the project has GDB configuration or large-content analysis
    /// is expected. Includes all 15 tools.
    Full {
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },

    /// Coding profile (default — no GDB, no context). 12 tools.
    ///
    /// For typical software engineering sessions without embedded debugging
    /// or large-file analysis. Leaner tools array caches more efficiently.
    Coding {
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },

    /// Research profile (read-only, no write tools). 8 tools.
    ///
    /// For exploration sessions where the agent should not modify files.
    /// No edit_file, write, shell (modifying commands), or task.
    Research {
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },

    /// Sub-agent tool set (Coding minus ask_question, minus task).
    ///
    /// Prevents unbounded nesting. Sub-agents should not spawn further
    /// sub-agents or interrupt the user with questions.
    SubAgent { todos: Arc<Mutex<Vec<TodoItem>>> },
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
    ) -> Self {
        if is_sub_agent {
            return ToolSetProfile::SubAgent { todos };
        }

        if mode == sven_config::AgentMode::Research {
            return ToolSetProfile::Research { question_tx, todos };
        }

        if has_gdb_config(project_root) {
            return ToolSetProfile::Full { question_tx, todos };
        }

        ToolSetProfile::Coding { question_tx, todos }
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
    use sven_tools::events::TodoItem;

    use super::{has_gdb_config, ToolSetProfile};

    fn todos() -> Arc<Mutex<Vec<TodoItem>>> {
        Arc::new(Mutex::new(vec![]))
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
        let profile = ToolSetProfile::detect(true, AgentMode::Agent, None, None, todos());
        assert_eq!(profile.name(), "subagent");
    }

    #[test]
    fn detect_research_mode_returns_research_profile() {
        let profile = ToolSetProfile::detect(false, AgentMode::Research, None, None, todos());
        assert_eq!(profile.name(), "research");
    }

    #[test]
    fn detect_with_gdb_config_returns_full_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gdbinit"), "").unwrap();
        let profile =
            ToolSetProfile::detect(false, AgentMode::Agent, Some(dir.path()), None, todos());
        assert_eq!(profile.name(), "full");
    }

    #[test]
    fn detect_default_returns_coding_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profile =
            ToolSetProfile::detect(false, AgentMode::Agent, Some(dir.path()), None, todos());
        assert_eq!(profile.name(), "coding");
    }

    #[test]
    fn detect_sub_agent_takes_priority_over_research_mode() {
        let profile = ToolSetProfile::detect(true, AgentMode::Research, None, None, todos());
        assert_eq!(
            profile.name(),
            "subagent",
            "sub-agent flag must take priority"
        );
    }
}
