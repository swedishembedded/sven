// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Command registry: central store for all registered slash commands.
//!
//! Built-in commands are registered at startup via [`CommandRegistry::with_builtins`].
//! User-authored commands discovered from `.cursor/commands/` directories are
//! registered via [`CommandRegistry::register_commands`].  Skills are intentionally
//! excluded from the slash command list — they are auto-loaded by the agent.

use std::collections::HashMap;
use std::sync::Arc;

use sven_runtime::{AgentInfo, SkillInfo};

use super::SlashCommand;

/// Central registry of all available slash commands.
///
/// Commands are stored as `Arc<dyn SlashCommand>` so they can be shared
/// between the registry and the completion manager without cloning.
pub struct CommandRegistry {
    commands: HashMap<String, Arc<dyn SlashCommand>>,
}

impl CommandRegistry {
    /// Create an empty registry.  Callers should then register built-in
    /// commands via [`register`] and optionally call the discovery methods.
    pub fn empty() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Create a registry pre-populated with all built-in commands.
    pub fn with_builtins() -> Self {
        use super::builtin;
        let mut reg = Self::empty();
        reg.register(Arc::new(builtin::abort::AbortCommand));
        reg.register(Arc::new(builtin::clear::ClearCommand));
        reg.register(Arc::new(builtin::model::ModelCommand));
        reg.register(Arc::new(builtin::provider::ProviderCommand));
        reg.register(Arc::new(builtin::mode::ModeCommand));
        reg.register(Arc::new(builtin::quit::QuitCommand));
        reg.register(Arc::new(builtin::refresh::RefreshCommand));
        reg
    }

    /// Register a command.  Replaces any existing command with the same name.
    pub fn register(&mut self, cmd: Arc<dyn SlashCommand>) {
        self.commands.insert(cmd.name().to_string(), cmd);
    }

    /// Look up a command by exact name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn SlashCommand>> {
        self.commands.get(name).cloned()
    }

    /// Iterate over all registered commands in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn SlashCommand>> + '_ {
        self.commands.values().cloned()
    }

    /// Return sorted list of command names (used by help and tab completion).
    // Not yet wired to a help/completion consumer; suppress until that is done.
    #[allow(dead_code)]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.commands.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    // ── Extension points ──────────────────────────────────────────────────────

    /// Register slash commands for all discovered user commands.
    ///
    /// Commands are `.md` files found in `commands/` directories (e.g.
    /// `.cursor/commands/`).  Each file becomes one slash command whose name
    /// mirrors its path relative to the commands root with the `.md` extension
    /// removed.  Hyphens in filenames are preserved (e.g. `review-code.md` →
    /// `/review-code`) to match the Cursor commands convention.
    pub fn register_commands(&mut self, commands: &[SkillInfo]) {
        for cmd in super::skill::make_command_slash_commands(commands) {
            self.register(Arc::new(cmd));
        }
    }

    /// Register slash commands for all discovered subagents.
    ///
    /// Each subagent markdown file found in `agents/` directories becomes one
    /// slash command.  The name is the lowercased agent `name` field (or file
    /// stem), with hyphens preserved (e.g. `security-auditor` →
    /// `/security-auditor`).  Model overrides from frontmatter are forwarded to
    /// the app via [`CommandResult::model_override`].
    pub fn register_agents(&mut self, agents: &[AgentInfo]) {
        for cmd in super::skill::make_agent_slash_commands(agents) {
            self.register(Arc::new(cmd));
        }
    }

    /// Query registered MCP servers for available prompts and register each
    /// prompt as a slash command.
    ///
    /// **Currently a stub** — returns without registering anything.
    /// Will be implemented when MCP integration is added.
    #[allow(dead_code)]
    pub async fn discover_mcp_prompts(&mut self) {
        let commands = super::mcp::discover_mcp_prompts().await;
        for cmd in commands {
            self.register(Arc::new(cmd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_builtins_registers_core_commands() {
        let reg = CommandRegistry::with_builtins();
        assert!(
            reg.get("model").is_some(),
            "model command must be registered"
        );
        assert!(
            reg.get("provider").is_some(),
            "provider command must be registered"
        );
        assert!(reg.get("mode").is_some(), "mode command must be registered");
        assert!(reg.get("quit").is_some(), "quit command must be registered");
        assert!(
            reg.get("abort").is_some(),
            "abort command must be registered"
        );
        assert!(
            reg.get("clear").is_some(),
            "clear command must be registered"
        );
    }

    #[test]
    fn register_replaces_existing_command() {
        use super::super::{CommandArgument, CommandContext, CommandResult};

        struct DummyCmd;
        impl SlashCommand for DummyCmd {
            fn name(&self) -> &str {
                "model"
            }
            fn description(&self) -> &str {
                "dummy"
            }
            fn arguments(&self) -> Vec<CommandArgument> {
                vec![]
            }
            fn complete(
                &self,
                _: usize,
                _: &str,
                _: &CommandContext,
            ) -> Vec<super::super::CompletionItem> {
                vec![]
            }
            fn execute(&self, _: Vec<String>) -> CommandResult {
                CommandResult::default()
            }
        }

        let mut reg = CommandRegistry::with_builtins();
        reg.register(Arc::new(DummyCmd));
        let cmd = reg.get("model").unwrap();
        assert_eq!(cmd.description(), "dummy");
    }

    #[test]
    fn names_returns_sorted_list() {
        let reg = CommandRegistry::with_builtins();
        let names = reg.names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "names() must return a sorted list");
    }
}
