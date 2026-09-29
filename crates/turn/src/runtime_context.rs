// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runtime context for an agent session.
//!
//! This is separate from [`sven_config::AgentConfig`], which holds only
//! config-file fields.  [`AgentRuntimeContext`] carries values detected or
//! specified at runtime (project root, git/CI context, prompt overrides,
//! discovered skills).

use std::path::PathBuf;

use sven_config::AgentMode;
use sven_model::Message;
use sven_workspace::{SharedAgents, SharedKnowledge, SharedSkills};

use crate::prompts::{system_prompt, PromptContext};

/// Environment-detected context injected into an agent at construction time.
#[derive(Debug, Default, Clone)]
pub struct AgentRuntimeContext {
    /// Absolute path to the project root (found via `.git` walk-up).
    pub project_root: Option<PathBuf>,
    /// Pre-formatted git context block (branch, commit, dirty status).
    pub git_context_note: Option<String>,
    /// Pre-formatted CI environment context block.
    pub ci_context_note: Option<String>,
    /// Path of the project context file (`.sven/context.md`, `AGENTS.md`, ...),
    /// when one exists. The system prompt references this path rather than
    /// inlining the file's content on every turn (see [`crate::prompts`]) -
    /// an agent that decides the file is relevant reads it itself with its
    /// normal file tool.
    pub project_context_file: Option<PathBuf>,
    /// Text appended to the default system prompt (from `--append-system-prompt`).
    pub append_system_prompt: Option<String>,
    /// Full system prompt override (from `--system-prompt-file`).
    /// When set, replaces `AgentConfig::system_prompt` entirely.
    pub system_prompt_override: Option<String>,
    /// Suppress Sven's built-in identity/guidelines/project-context prompt
    /// (from `--no-system`). See [`Self::build_system_message`] for exact
    /// semantics when combined with `system_prompt_override`/`append_system_prompt`.
    pub no_system: bool,
    /// Suppress tool availability entirely (from `--no-tools`): no tool
    /// schemas are sent to the model and any tool call is refused. Consumed
    /// by `RuntimeBuilder` (wires `TurnExecutor`/`ToolExecutor`), not by
    /// `build_system_message` - independent of `no_system`.
    pub no_tools: bool,
    /// Skills discovered from the standard search hierarchy.
    ///
    /// Held as [`SharedSkills`] so the TUI can trigger a live refresh (via
    /// `/refresh`) and the next agent turn automatically picks up new skills
    /// when rebuilding the system prompt.
    pub skills: SharedSkills,
    /// Subagents discovered from the standard search hierarchy.
    ///
    /// Held as [`SharedAgents`] so the TUI can trigger a live refresh and the
    /// next agent turn picks up new subagents when rebuilding the system prompt.
    pub agents: SharedAgents,
    /// Knowledge documents discovered from `.sven/knowledge/`.
    ///
    /// Held as [`SharedKnowledge`] for live-refresh parity with skills and
    /// agents.  The `list_knowledge` and `search_knowledge` tools hold a clone
    /// of this and serve reads without touching the filesystem on every call.
    pub knowledge: SharedKnowledge,
    /// Pre-formatted knowledge drift warning block, computed once at session
    /// start.  Injected into the system prompt so the agent is immediately
    /// aware of subsystems whose documentation may be stale.
    pub knowledge_drift_note: Option<String>,
    /// Prior conversation messages to pre-load into the session history.
    ///
    /// Used when resuming a stored conversation: the tail of the local
    /// conversation store is loaded and injected here so the agent has
    /// context from previous turns.
    pub prior_messages: Vec<Message>,
}

impl AgentRuntimeContext {
    /// Build the system message to seed a fresh session's conversation thread,
    /// honouring `no_system`, `system_prompt_override`, and `append_system_prompt`.
    ///
    /// - Default (`no_system` unset): the full built-in identity/guidelines
    ///   prompt (or `system_prompt_override` in its place), plus project/git/CI
    ///   context, skills, agents, and knowledge — exactly what
    ///   [`crate::prompts::system_prompt`] produces.
    /// - `no_system` with neither override nor append text: `None` — no system
    ///   message at all, so zero tokens are spent before the first message.
    /// - `no_system` with `system_prompt_override` set: the override text
    ///   verbatim (plus `append_system_prompt` if also given), with none of
    ///   Sven's built-in identity/guidelines/context sections.
    /// - `no_system` with only `append_system_prompt` set (no override): the
    ///   appended text alone becomes the entire system message.
    #[must_use]
    pub fn build_system_message(&self, mode: AgentMode) -> Option<Message> {
        if self.no_system
            && self.system_prompt_override.is_none()
            && self.append_system_prompt.is_none()
        {
            return None;
        }

        let text = if self.no_system {
            match (&self.system_prompt_override, &self.append_system_prompt) {
                (Some(custom), Some(extra)) => format!("{}\n\n{extra}", custom.trim_end()),
                (Some(custom), None) => custom.clone(),
                (None, Some(extra)) => extra.clone(),
                (None, None) => unreachable!("checked above"),
            }
        } else {
            let ctx = PromptContext {
                project_root: self.project_root.as_deref(),
                git_context: self.git_context_note.as_deref(),
                project_context_file: self.project_context_file.as_deref(),
                ci_context: self.ci_context_note.as_deref(),
                append: self.append_system_prompt.as_deref(),
                skills: self.skills.get(),
                agents: self.agents.get(),
                knowledge: self.knowledge.get(),
                knowledge_drift_note: self.knowledge_drift_note.as_deref(),
            };
            system_prompt(mode, self.system_prompt_override.as_deref(), ctx)
        };

        Some(Message::system(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_context_builds_the_full_default_prompt() {
        let ctx = AgentRuntimeContext::default();
        let msg = ctx
            .build_system_message(AgentMode::Agent)
            .expect("default prompt");
        let text = msg.as_text().unwrap();
        assert!(text.contains("You are Sven"));
        assert!(text.contains("## Guidelines"));
    }

    #[test]
    fn no_system_alone_produces_no_message() {
        let ctx = AgentRuntimeContext {
            no_system: true,
            ..Default::default()
        };
        assert!(ctx.build_system_message(AgentMode::Agent).is_none());
    }

    #[test]
    fn no_system_with_override_uses_override_verbatim() {
        let ctx = AgentRuntimeContext {
            no_system: true,
            system_prompt_override: Some("You are a test bot.".to_string()),
            ..Default::default()
        };
        let msg = ctx
            .build_system_message(AgentMode::Agent)
            .expect("override prompt");
        let text = msg.as_text().unwrap();
        assert_eq!(text, "You are a test bot.");
        assert!(!text.contains("You are Sven"));
    }

    #[test]
    fn no_system_with_override_and_append_combines_both() {
        let ctx = AgentRuntimeContext {
            no_system: true,
            system_prompt_override: Some("Base.".to_string()),
            append_system_prompt: Some("Extra.".to_string()),
            ..Default::default()
        };
        let msg = ctx
            .build_system_message(AgentMode::Agent)
            .expect("combined prompt");
        assert_eq!(msg.as_text().unwrap(), "Base.\n\nExtra.");
    }

    #[test]
    fn no_system_with_only_append_uses_append_alone() {
        let ctx = AgentRuntimeContext {
            no_system: true,
            append_system_prompt: Some("Just this.".to_string()),
            ..Default::default()
        };
        let msg = ctx
            .build_system_message(AgentMode::Agent)
            .expect("append-only prompt");
        assert_eq!(msg.as_text().unwrap(), "Just this.");
    }

    #[test]
    fn override_without_no_system_still_replaces_default() {
        let ctx = AgentRuntimeContext {
            system_prompt_override: Some("Custom only.".to_string()),
            ..Default::default()
        };
        let msg = ctx
            .build_system_message(AgentMode::Agent)
            .expect("override prompt");
        assert_eq!(msg.as_text().unwrap(), "Custom only.");
    }
}
