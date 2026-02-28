// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
#![allow(dead_code)]
//! MCP-prompt-based slash commands.
//!
//! **This module is a stub.**  The discovery function currently returns an
//! empty list.  It will be implemented when MCP integration is added.
//!
//! Planned behaviour:
//! - Query each configured MCP server for its list of prompts
//! - Register each prompt as a `/prompt-name [args]` slash command
//! - Argument completions driven by the MCP prompt argument schema

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

/// A slash command backed by an MCP prompt.
pub struct McpPromptCommand {
    pub prompt_name: String,
    pub server_id: String,
    // Future fields: argument schema, description from MCP metadata
}

impl SlashCommand for McpPromptCommand {
    fn name(&self) -> &str {
        &self.prompt_name
    }

    fn description(&self) -> &str {
        "MCP prompt command"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        // Future: derive from MCP prompt argument schema
        vec![]
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        // Future: use MCP argument schema for completions, query resources, etc.
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        // Future: invoke the MCP prompt and return result as message_to_send
        CommandResult::default()
    }
}

/// Query configured MCP servers for available prompts.
///
/// **Currently returns an empty vec** (stub implementation).
pub async fn discover_mcp_prompts() -> Vec<McpPromptCommand> {
    // TODO: implement when MCP integration is added
    vec![]
}
