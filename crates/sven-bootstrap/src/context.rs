// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Runtime context types for agent construction.
//!
//! [`RuntimeContext`] holds environment-detected information (project root,
//! git state, CI environment) that is not part of the config file schema.
//!
//! [`ToolSetProfile`] selects which tools to register, and carries the
//! shared state needed by stateful tools (todos, mode lock, GDB state).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use tokio::sync::{mpsc, Mutex};

use sven_tools::{
    events::TodoItem,
    QuestionRequest,
};
use sven_runtime::{CiContext, GitContext};

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
}

impl RuntimeContext {
    /// Create with auto-detected project, git, and CI context.
    pub fn auto_detect() -> Self {
        let project_root = sven_runtime::find_project_root().ok();
        let git_context = project_root.as_ref()
            .map(|r| sven_runtime::collect_git_context(r));
        let ci_context = Some(sven_runtime::detect_ci_context());
        let project_context_file = project_root.as_ref()
            .and_then(|r| sven_runtime::load_project_context_file(r));

        Self {
            project_root,
            git_context,
            ci_context,
            project_context_file,
            append_system_prompt: None,
            system_prompt_override: None,
        }
    }

    /// Create an empty context (no project/git/CI detection).
    pub fn empty() -> Self {
        Self::default()
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
        task_depth: Arc<AtomicUsize>,
    },

    /// Sub-agent tool set (Full minus TaskTool to prevent unbounded nesting).
    SubAgent {
        todos: Arc<Mutex<Vec<TodoItem>>>,
    },
}
