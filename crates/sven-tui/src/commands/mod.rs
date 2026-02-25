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
    /// Override the model for the next queued message (e.g. `"anthropic/claude-opus-4-6"`).
    pub model_override: Option<String>,

    /// Override the agent mode for the next queued message.
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
