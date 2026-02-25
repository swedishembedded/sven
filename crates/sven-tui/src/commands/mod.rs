// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Slash command system for the interactive TUI.
//!
//! Commands are invoked by typing `/command [args]` in the input box.
//! The system is designed to be extensible: built-in commands (model, mode,
//! provider, quit) are registered at startup; future extensions will add MCP
//! prompts and SKILL.md-based commands via the same registry.

pub mod builtin;
pub mod completion;
pub mod mcp;
pub mod parser;
pub mod registry;
pub mod skill;

pub use completion::{CompletionItem, CompletionManager};
pub use parser::{parse, ParsedCommand};
pub use registry::CommandRegistry;

use sven_config::{AgentMode, Config};
use std::sync::Arc;

// ── Context ───────────────────────────────────────────────────────────────────

/// Context passed to commands when generating completions.
///
/// Provides read-only access to configuration and current session state.
/// Does not include mutable app state — commands return effects via
/// [`CommandResult`] rather than mutating state directly.
#[allow(dead_code)]
pub struct CommandContext {
    pub config: Arc<Config>,
    /// Provider of the currently active model (e.g. `"openai"`).
    /// Available to commands that want to highlight the active model in completions.
    pub current_model_provider: String,
    /// Name of the currently active model (e.g. `"gpt-4o"`).
    /// Available to commands that want to highlight the active model in completions.
    pub current_model_name: String,
}

// ── Results ───────────────────────────────────────────────────────────────────

/// The effect(s) a command wants to produce when executed.
///
/// Commands do not mutate app state directly; they return this struct and the
/// app applies each effect.  This keeps commands stateless and testable.
#[derive(Debug, Default)]
pub struct CommandResult {
    /// Permanently switch the model starting from the next message
    /// (e.g. `"anthropic/claude-opus-4-6"`).  The switch persists for all
    /// subsequent messages — it is not reverted after the turn completes.
    pub model_override: Option<String>,

    /// Permanently switch the agent mode starting from the next message.
    pub mode_override: Option<AgentMode>,

    /// If set, the command wants to send this text as the user message.
    /// If `None` the command only updates overrides and sends nothing.
    pub message_to_send: Option<String>,

    /// If set, triggers an immediate side-effect in the app (e.g. quit).
    pub immediate_action: Option<ImmediateAction>,
}

/// Side-effects that must be handled by the app immediately (before queuing).
#[derive(Debug)]
pub enum ImmediateAction {
    Quit,
    // Future: RefreshMcp, ReloadSkills, ShowHelp, etc.
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A slash command that can be invoked from the input box.
///
/// Implementations must be `Send + Sync` so they can be stored in the
/// registry behind an `Arc`.
pub trait SlashCommand: Send + Sync {
    /// The command keyword used after `/` (e.g. `"model"` for `/model`).
    fn name(&self) -> &str;

    /// One-line description shown in completion list and help.
    fn description(&self) -> &str;

    /// Metadata about expected arguments.
    ///
    /// Used for help text generation and future shell-completion export.
    /// Not called by the completion engine itself.
    #[allow(dead_code)]
    fn arguments(&self) -> Vec<CommandArgument> {
        vec![]
    }

    /// Generate completions for the argument at `arg_index` given `partial`
    /// text typed so far.
    ///
    /// `arg_index = 0` means the first argument after the command name.
    /// Implementations should return an empty vec when no completions apply.
    ///
    /// The default implementation returns an empty vec (no completions).
    fn complete(&self, arg_index: usize, partial: &str, ctx: &CommandContext) -> Vec<CompletionItem>;

    /// Execute the command with the given arguments.
    ///
    /// Returns a [`CommandResult`] describing the effects to apply.
    fn execute(&self, args: Vec<String>) -> CommandResult;
}

// ── Dispatch helper ───────────────────────────────────────────────────────────

/// Dispatch `input` as a slash command against `registry`.
///
/// This is the primary test surface for slash-command dispatch.  All command
/// integration tests should call this function rather than driving the full TUI.
///
/// `ctx` provides session context (current model/mode) that commands may use
/// for completion and contextual defaults.  Pass a default-constructed
/// `CommandContext` in tests where session state does not matter.
///
/// Returns `Some((command_name, result))` when a known command was matched and
/// executed; `None` when the input is not a slash command or the command name
/// is not registered.
///
/// **Enter semantics**: pressing Enter finalises whatever is being typed.
/// - `"/model gpt-4o"` (no trailing space) → `CompletingArgs { partial: "gpt-4o" }` → executes with `["gpt-4o"]`
/// - `"/model gpt-4o "` (trailing space)   → `Complete { args: ["gpt-4o"] }` → same result
/// - `"/quit"`                              → `PartialCommand` → executes with `[]`
pub fn dispatch_command(
    input: &str,
    registry: &CommandRegistry,
    _ctx: &CommandContext,
) -> Option<(String, CommandResult)> {
    try_dispatch(input, registry)
}

/// Legacy alias for [`dispatch_command`] without a context argument.
///
/// Prefer `dispatch_command` for new call sites.
pub fn try_dispatch(input: &str, registry: &CommandRegistry) -> Option<(String, CommandResult)> {
    let parsed = parse(input);
    if matches!(parsed, ParsedCommand::NotCommand) {
        return None;
    }

    let (cmd_name, cmd_args): (String, Vec<String>) = match &parsed {
        ParsedCommand::Complete { command, args } => (command.clone(), args.clone()),
        ParsedCommand::PartialCommand { partial } => (partial.clone(), vec![]),
        // Enter finalises the partial as the last argument.
        // Built-in commands have exactly one arg at index 0, so this always
        // produces the correct `[partial]` list for them.
        ParsedCommand::CompletingArgs { command, arg_index, partial } => {
            let mut args: Vec<String> = (0..*arg_index).map(|_| String::new()).collect();
            if !partial.is_empty() {
                args.push(partial.clone());
            }
            (command.clone(), args)
        }
        ParsedCommand::NotCommand => return None,
    };

    let cmd = registry.get(&cmd_name)?;
    Some((cmd_name, cmd.execute(cmd_args)))
}

// ── Argument metadata ─────────────────────────────────────────────────────────

/// Describes one argument expected by a slash command.
///
/// Returned by [`SlashCommand::arguments`] and used for help text generation,
/// argument count validation, and future shell-completion export.
/// Not all fields are used yet; they are part of the extension-ready API.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommandArgument {
    /// Short name shown in usage hint (e.g. `"model"`).
    pub name: &'static str,
    /// Brief description.
    pub description: &'static str,
    /// Whether the command requires this argument to be present.
    pub required: bool,
}

impl CommandArgument {
    #[allow(dead_code)]
    pub const fn required(name: &'static str, description: &'static str) -> Self {
        Self { name, description, required: true }
    }

    #[allow(dead_code)]
    pub const fn optional(name: &'static str, description: &'static str) -> Self {
        Self { name, description, required: false }
    }
}

// ── Dispatch integration tests ────────────────────────────────────────────────
//
// These tests exercise the full parse → arg-build → execute pipeline through
// `try_dispatch`, covering every input variation that the `Action::Submit`
// handler in `App::dispatch` must handle correctly.

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn registry() -> CommandRegistry {
        CommandRegistry::with_builtins()
    }

    // ── /model ────────────────────────────────────────────────────────────────

    /// The primary regression: typing "/model gpt-4o" without a trailing space
    /// and pressing Enter must set the model override.  Before the fix, the
    /// `CompletingArgs` variant was silently dropped and nothing happened.
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
        assert_eq!(result.model_override.as_deref(), Some("anthropic/claude-opus-4-6"));
    }

    #[test]
    fn model_provider_slash_model_with_trailing_space() {
        let (_, result) = try_dispatch("/model anthropic/claude-opus-4-6 ", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("anthropic/claude-opus-4-6"));
    }

    #[test]
    fn model_openai_catalog_form() {
        let (_, result) = try_dispatch("/model openai/gpt-4o", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("openai/gpt-4o"));
    }

    #[test]
    fn model_named_custom_provider() {
        let (_, result) = try_dispatch("/model my_ollama", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("my_ollama"));
    }

    /// "/model " with only a space — no model arg → override must NOT be set.
    #[test]
    fn model_bare_command_no_arg_no_override() {
        let (_, result) = try_dispatch("/model ", &registry()).unwrap();
        assert!(result.model_override.is_none(), "bare /model must not set override");
    }

    // ── /mode ─────────────────────────────────────────────────────────────────

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
    fn mode_bare_command_no_arg_no_override() {
        let (_, result) = try_dispatch("/mode ", &registry()).unwrap();
        assert!(result.mode_override.is_none());
    }

    // ── /provider ─────────────────────────────────────────────────────────────

    #[test]
    fn provider_sets_model_override_to_provider_id() {
        let (name, result) = try_dispatch("/provider anthropic", &registry()).unwrap();
        assert_eq!(name, "provider");
        // provider command sets model_override to just the provider id;
        // resolve_model_from_config then keeps the current model name.
        assert_eq!(result.model_override.as_deref(), Some("anthropic"));
    }

    #[test]
    fn provider_openai_no_trailing_space() {
        let (_, result) = try_dispatch("/provider openai", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("openai"));
    }

    #[test]
    fn provider_with_trailing_space() {
        let (_, result) = try_dispatch("/provider ollama ", &registry()).unwrap();
        assert_eq!(result.model_override.as_deref(), Some("ollama"));
    }

    #[test]
    fn provider_bare_command_no_arg_no_override() {
        let (_, result) = try_dispatch("/provider ", &registry()).unwrap();
        assert!(result.model_override.is_none());
    }

    // ── /quit ─────────────────────────────────────────────────────────────────

    /// "/quit" without trailing space — PartialCommand path.
    #[test]
    fn quit_no_trailing_space_triggers_quit() {
        let (name, result) = try_dispatch("/quit", &registry()).unwrap();
        assert_eq!(name, "quit");
        assert!(matches!(result.immediate_action, Some(ImmediateAction::Quit)));
    }

    /// "/quit " with trailing space — CompletingArgs path (arg 0, empty partial).
    #[test]
    fn quit_with_trailing_space_triggers_quit() {
        let (_, result) = try_dispatch("/quit ", &registry()).unwrap();
        assert!(matches!(result.immediate_action, Some(ImmediateAction::Quit)));
    }

    // ── Non-commands ──────────────────────────────────────────────────────────

    #[test]
    fn regular_text_returns_none() {
        assert!(try_dispatch("hello world", &registry()).is_none());
    }

    #[test]
    fn empty_input_returns_none() {
        assert!(try_dispatch("", &registry()).is_none());
    }

    #[test]
    fn bare_slash_returns_none() {
        // "/" alone — PartialCommand with empty name — no registered command named ""
        assert!(try_dispatch("/", &registry()).is_none());
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(try_dispatch("/nonexistent gpt-4o", &registry()).is_none());
    }

    #[test]
    fn unknown_command_bare_returns_none() {
        assert!(try_dispatch("/xyz", &registry()).is_none());
    }

    // ── Result side-effect isolation ──────────────────────────────────────────

    #[test]
    fn model_command_does_not_set_mode_or_quit() {
        let (_, result) = try_dispatch("/model gpt-4o", &registry()).unwrap();
        assert!(result.mode_override.is_none());
        assert!(result.immediate_action.is_none());
        assert!(result.message_to_send.is_none());
    }

    #[test]
    fn mode_command_does_not_set_model_or_quit() {
        let (_, result) = try_dispatch("/mode plan", &registry()).unwrap();
        assert!(result.model_override.is_none());
        assert!(result.immediate_action.is_none());
        assert!(result.message_to_send.is_none());
    }

    #[test]
    fn quit_command_does_not_set_model_or_mode() {
        let (_, result) = try_dispatch("/quit", &registry()).unwrap();
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }
}
