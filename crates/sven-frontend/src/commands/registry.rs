// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Command registry: central store for all registered slash commands.

use std::collections::HashMap;
use std::sync::Arc;

use sven_runtime::{AgentInfo, SkillInfo};

use super::SlashCommand;

/// Central registry of all available slash commands.
pub struct CommandRegistry {
    commands: HashMap<String, Arc<dyn SlashCommand>>,
}

impl CommandRegistry {
    /// Create an empty registry.
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
        reg.register(Arc::new(builtin::new::NewCommand));
        reg.register(Arc::new(builtin::provider::ProviderCommand));
        reg.register(Arc::new(builtin::mode::ModeCommand));
        reg.register(Arc::new(builtin::quit::QuitCommand));
        reg.register(Arc::new(builtin::refresh::RefreshCommand));
        reg.register(Arc::new(builtin::team::ApproveCommand));
        reg.register(Arc::new(builtin::team::RejectCommand));
        reg.register(Arc::new(builtin::team::AgentsCommand));
        reg.register(Arc::new(builtin::team::TasksCommand));
        reg.register(Arc::new(builtin::team::ArchitectCommand));
        reg.register(Arc::new(builtin::inspect::SkillsCommand));
        reg.register(Arc::new(builtin::inspect::SubagentsCommand));
        reg.register(Arc::new(builtin::inspect::PeersCommand));
        reg.register(Arc::new(builtin::inspect::ContextCommand));
        reg.register(Arc::new(builtin::inspect::ToolsCommand));
        reg.register(Arc::new(builtin::inspect::McpCommand));
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

    /// Return sorted list of command names.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.commands.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Register slash commands for all discovered user commands.
    pub fn register_commands(&mut self, commands: &[SkillInfo]) {
        for cmd in super::skill::make_command_slash_commands(commands) {
            self.register(Arc::new(cmd));
        }
    }

    /// Register slash commands for all discovered subagents.
    pub fn register_agents(&mut self, agents: &[AgentInfo]) {
        for cmd in super::skill::make_agent_slash_commands(agents) {
            self.register(Arc::new(cmd));
        }
    }

    /// Query the `McpManager` for available prompts and register each as a slash command.
    pub async fn register_mcp_prompts(
        &mut self,
        manager: &std::sync::Arc<sven_mcp_client::McpManager>,
    ) {
        let commands = super::mcp::discover_mcp_prompts(manager).await;
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
        assert!(reg.get("model").is_some());
        assert!(reg.get("provider").is_some());
        assert!(reg.get("mode").is_some());
        assert!(reg.get("quit").is_some());
        assert!(reg.get("abort").is_some());
        assert!(reg.get("clear").is_some());
    }

    #[test]
    fn names_returns_sorted_list() {
        let reg = CommandRegistry::with_builtins();
        let names = reg.names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }
}
