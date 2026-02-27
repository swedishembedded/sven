// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Command registry: central store for all registered slash commands.
//!
//! Built-in commands are registered at startup.  Future extension points
//! for MCP prompts and Skills are exposed as async discovery methods that
//! currently return immediately (stubs to be filled in later).

use std::collections::HashMap;
use std::sync::Arc;

use sven_runtime::SkillInfo;

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
        Self { commands: HashMap::new() }
    }

    /// Create a registry pre-populated with all built-in commands.
    pub fn with_builtins() -> Self {
        use super::builtin;
        let mut reg = Self::empty();
        reg.register(Arc::new(builtin::abort::AbortCommand));
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

    /// Register slash commands for all discovered skills.
    ///
    /// Each skill in `skills` becomes a `/skill-name` command.  Skills with
    /// `user_invocable_only: true` are included (slash commands are explicitly
    /// user-invoked).  Duplicate sanitized names are disambiguated with `_2`,
    /// `_3`, etc.
    pub fn register_skills(&mut self, skills: &[SkillInfo]) {
        for cmd in super::skill::make_skill_commands(skills) {
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
        assert!(reg.get("model").is_some(), "model command must be registered");
        assert!(reg.get("provider").is_some(), "provider command must be registered");
        assert!(reg.get("mode").is_some(), "mode command must be registered");
        assert!(reg.get("quit").is_some(), "quit command must be registered");
        assert!(reg.get("abort").is_some(), "abort command must be registered");
    }

    #[test]
    fn register_replaces_existing_command() {
        use super::super::{CommandArgument, CommandContext, CommandResult};

        struct DummyCmd;
        impl SlashCommand for DummyCmd {
            fn name(&self) -> &str { "model" }
            fn description(&self) -> &str { "dummy" }
            fn arguments(&self) -> Vec<CommandArgument> { vec![] }
            fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<super::super::CompletionItem> { vec![] }
            fn execute(&self, _: Vec<String>) -> CommandResult { CommandResult::default() }
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
