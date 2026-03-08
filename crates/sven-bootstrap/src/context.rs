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

/// Selects which tool set to register and carries the caller-owned shared
/// state that stateful tools require.
///
/// TUI and headless/CI use the same full tool set; only `--mode` (research /
/// plan / agent) controls which tools are exposed to the model. When
/// `question_tx` is `Some`, ask_question uses the TUI channel; when `None`,
/// it uses stdin (headless/CI).
///
/// `mode_lock` and the tool-event channel are intentionally **not** part of
/// this enum — `AgentBuilder::build()` creates them, wires them into the
/// registry, and passes the same instances to `Agent::new()` so that
/// `SwitchModeTool` and `TodoWriteTool` events are correctly observed by the
/// agent loop.
pub enum ToolSetProfile {
    /// Full tool set (TUI and headless/CI). Same tools; mode gates visibility.
    ///
    /// `question_tx`: when `Some`, ask_question routes to the TUI; when `None`, uses stdin.
    Full {
        question_tx: Option<mpsc::Sender<QuestionRequest>>,
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },

    /// Sub-agent tool set (Full minus TaskTool to prevent unbounded nesting).
    SubAgent { todos: Arc<Mutex<Vec<TodoItem>>> },
}
