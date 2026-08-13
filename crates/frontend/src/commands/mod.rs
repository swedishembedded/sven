// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash command system for Sven frontends.
//!
//! Commands are invoked by typing `/command [args]` in the input box.
//! The system is extensible: built-in commands are registered at startup;
//! skill files, subagents, and MCP prompts extend the registry at runtime.
//!
//! This module is shared between sven-tui and sven-gui via sven-frontend.

pub mod builtin;
pub mod completion;
pub mod mcp;
pub mod parser;
pub mod registry;
pub mod skill;

pub use completion::{CompletionItem, CompletionManager};
pub use parser::{parse, ParsedCommand};
pub use registry::CommandRegistry;

use std::sync::Arc;
use sven_config::{AgentMode, Config};

// ── Inspector kind ────────────────────────────────────────────────────────────

/// Identifies which inspector view to open.
///
/// Used by `ImmediateAction::OpenInspector` to select the content rendered
/// in the full-screen inspector overlay (TUI) or inspector panel (GUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorKind {
    Skills,
    Subagents,
    Peers,
    Context,
    Tools,
    Mcp,
}

impl InspectorKind {
    /// Header title for the inspector.
    pub fn title(self) -> &'static str {
        match self {
            InspectorKind::Skills => "SKILLS",
            InspectorKind::Subagents => "SUBAGENTS",
            InspectorKind::Peers => "PEERS",
            InspectorKind::Context => "CONTEXT",
            InspectorKind::Tools => "TOOLS",
            InspectorKind::Mcp => "MCP SERVERS",
        }
    }
}

// ── Context ───────────────────────────────────────────────────────────────────

/// Context passed to commands when generating completions.
pub struct CommandContext {
    pub config: Arc<Config>,
    pub current_model_provider: String,
    pub current_model_name: String,
}

// ── Results ───────────────────────────────────────────────────────────────────

/// The effect(s) a command wants to produce when executed.
#[derive(Debug, Default)]
pub struct CommandResult {
    pub model_override: Option<String>,
    pub mode_override: Option<AgentMode>,
    pub message_to_send: Option<String>,
    pub immediate_action: Option<ImmediateAction>,
}

/// Side-effects that must be handled by the app immediately (before queuing).
#[derive(Debug)]
pub enum ImmediateAction {
    Quit,
    Abort,
    RefreshSkills,
    ClearChat,
    NewConversation,
    ApprovePlan {
        task_id: String,
    },
    RejectPlan {
        task_id: String,
        feedback: String,
    },
    OpenTeamPicker,
    ToggleTaskList,
    OpenInspector {
        kind: InspectorKind,
    },
    McpAuth {
        server: String,
    },
    /// Switch the operator console's tenant scope; `None` = all tenants.
    SelectTenant {
        tenant: Option<String>,
    },
    /// Surface share/handoff instructions to the user (a notice, not a message
    /// to the agent). Emitted by `/share --print` or when no broker is
    /// configured; the frontend shows `text`.
    ShareInstructions {
        text: String,
    },
    /// One-tap `/share`: expose THIS running session to the configured broker.
    /// The frontend forwards these options to the live session's agent task as
    /// an `AgentRequest::ShareSession`, which starts the in-process share
    /// bridge. Carries the broker URL/token/tenant resolved from the
    /// environment plus the (optional) share id and title.
    ShareSession {
        options: Box<crate::share::FrontendShareOptions>,
    },
    /// `/think-limit <args>`: set (`Some`) or clear (`None`, reverting to the
    /// `agent.max_thinking_tokens`/`agent.thinking_timeout_secs` config
    /// values) the process-wide thinking-loop watchdog override
    /// (`sven_machines::stream_turn::set_thinking_budget_override`). Live for
    /// every session/subagent in this process starting with the next turn -
    /// no session rebuild needed, unlike `/model`.
    SetThinkingBudget(Option<sven_machines::ThinkingBudget>),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A slash command that can be invoked from the input box.
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        ctx: &CommandContext,
    ) -> Vec<CompletionItem>;

    fn execute(&self, args: Vec<String>) -> CommandResult;
}

// ── Dispatch helper ───────────────────────────────────────────────────────────

/// Dispatch `input` as a slash command against `registry`.
pub fn dispatch_command(
    input: &str,
    registry: &CommandRegistry,
    _ctx: &CommandContext,
) -> Option<(String, CommandResult)> {
    try_dispatch(input, registry)
}

/// Dispatch without a context argument.
pub fn try_dispatch(input: &str, registry: &CommandRegistry) -> Option<(String, CommandResult)> {
    let parsed = parse(input);
    if matches!(parsed, ParsedCommand::NotCommand) {
        return None;
    }

    let (cmd_name, cmd_args): (String, Vec<String>) = match &parsed {
        ParsedCommand::Complete { command, args } => (command.clone(), args.clone()),
        ParsedCommand::PartialCommand { partial } => (partial.clone(), vec![]),
        ParsedCommand::CompletingArgs { command, .. } => {
            let all_tokens = parser::tokenise(&input[1..]);
            let args = all_tokens.into_iter().skip(1).collect();
            (command.clone(), args)
        }
        ParsedCommand::NotCommand => return None,
    };

    let cmd = registry.get(&cmd_name)?;
    Some((cmd_name, cmd.execute(cmd_args)))
}

// ── Dispatch integration tests ────────────────────────────────────────────────

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn registry() -> CommandRegistry {
        CommandRegistry::with_builtins()
    }

    #[test]
    fn model_no_trailing_space_sets_override() {
        let (name, result) = try_dispatch("/model gpt-4o", &registry()).unwrap();
        assert_eq!(name, "model");
        assert_eq!(result.model_override.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn model_with_trailing_space_sets_override() {
        let (_, result) = try_dispatch("/model gpt-4o ", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn model_provider_slash_model_no_trailing_space() {
        let (_, result) = try_dispatch("/model anthropic/claude-opus-4-6", &registry()).unwrap();
        assert_eq!(
            result.model_override.as_deref(),
            Some("anthropic/claude-opus-4-6")
        );
    }

    #[test]
    fn mode_research_no_trailing_space() {
        let (name, result) = try_dispatch("/mode research", &registry()).unwrap();
        assert_eq!(name, "mode");
        assert_eq!(result.mode_override, Some(AgentMode::Research));
    }

    #[test]
    fn mode_plan_with_trailing_space() {
        let (_, result) = try_dispatch("/mode plan ", &registry()).unwrap();
        assert_eq!(result.mode_override, Some(AgentMode::Plan));
    }

    #[test]
    fn mode_agent_no_trailing_space() {
        let (_, result) = try_dispatch("/mode agent", &registry()).unwrap();
        assert_eq!(result.mode_override, Some(AgentMode::Agent));
    }

    #[test]
    fn mode_invalid_value_returns_no_override() {
        let (_, result) = try_dispatch("/mode invalid_mode", &registry()).unwrap();
        assert!(result.mode_override.is_none());
    }

    #[test]
    fn quit_no_trailing_space_triggers_quit() {
        let (name, result) = try_dispatch("/quit", &registry()).unwrap();
        assert_eq!(name, "quit");
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Quit)
        ));
    }

    #[test]
    fn clear_no_trailing_space_triggers_clear() {
        let (name, result) = try_dispatch("/clear", &registry()).unwrap();
        assert_eq!(name, "clear");
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::ClearChat)
        ));
    }

    #[test]
    fn abort_no_trailing_space_triggers_abort() {
        let (name, result) = try_dispatch("/abort", &registry()).unwrap();
        assert_eq!(name, "abort");
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Abort)
        ));
    }

    #[test]
    fn skills_triggers_open_inspector_skills() {
        let (name, result) = try_dispatch("/skills", &registry()).unwrap();
        assert_eq!(name, "skills");
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Skills
            })
        ));
    }

    #[test]
    fn mcp_auth_triggers_mcp_auth_action() {
        let (name, result) = try_dispatch("/mcp auth atlassian-mcp", &registry()).unwrap();
        assert_eq!(name, "mcp");
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::McpAuth { ref server }) if server == "atlassian-mcp"
        ));
    }

    #[test]
    fn regular_text_returns_none() {
        assert!(try_dispatch("hello world", &registry()).is_none());
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(try_dispatch("/nonexistent gpt-4o", &registry()).is_none());
    }
}
