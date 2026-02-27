// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Runtime context for an agent session.
//!
//! This is separate from [`sven_config::AgentConfig`], which holds only
//! config-file fields.  [`AgentRuntimeContext`] carries values detected or
//! specified at runtime (project root, git/CI context, prompt overrides,
//! discovered skills).

use std::path::PathBuf;
use std::sync::Arc;

use sven_runtime::SkillInfo;

/// Environment-detected context injected into an agent at construction time.
#[derive(Debug, Default, Clone)]
pub struct AgentRuntimeContext {
    /// Absolute path to the project root (found via `.git` walk-up).
    pub project_root: Option<PathBuf>,
    /// Pre-formatted git context block (branch, commit, dirty status).
    pub git_context_note: Option<String>,
    /// Pre-formatted CI environment context block.
    pub ci_context_note: Option<String>,
    /// Contents of the project context file (`.sven/context.md`, `AGENTS.md`, …).
    pub project_context_file: Option<String>,
    /// Text appended to the default system prompt (from `--append-system-prompt`).
    pub append_system_prompt: Option<String>,
    /// Full system prompt override (from `--system-prompt-file`).
    /// When set, replaces `AgentConfig::system_prompt` entirely.
    pub system_prompt_override: Option<String>,
    /// Skills discovered from the standard search hierarchy.
    /// Shared with `LoadSkillTool` via `Arc` to avoid cloning skill content.
    pub skills: Arc<[SkillInfo]>,
}
