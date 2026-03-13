// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP-prompt-based slash commands.
//!
//! Each MCP prompt discovered from a connected server is registered as a
//! slash command `/servername/promptname [args...]`.  When executed the
//! command fetches the rendered prompt from the server and injects it as
//! the next user message, optionally filling in argument values.
//!
//! # Discovery
//!
//! Call [`register_mcp_prompts`] after the `McpManager` is available to
//! populate the `CommandRegistry` with one `McpPromptCommand` per prompt.
//! Re-call after a server reconnects (e.g. when `/refresh` is used) to pick
//! up newly discovered prompts.

use std::sync::Arc;

use sven_mcp_client::{McpManager, McpPromptArgInfo, McpPromptInfo};

use crate::commands::{CommandContext, CommandResult, CompletionItem, SlashCommand};

// ── McpPromptCommand ──────────────────────────────────────────────────────────

/// Slash command that fetches and injects an MCP prompt as a user message.
///
/// The command name is `<server_name>/<prompt_name>` (with `/` preserved so
/// completion works naturally).  Argument values are passed positionally.
pub struct McpPromptCommand {
    /// Full slash-command name: `<server>/<prompt>`.
    name: String,
    /// Short description from the MCP server metadata.
    description: String,
    /// Argument definitions for tab completion.
    args: Vec<McpPromptArgInfo>,
    /// MCP server name (used when calling `get_prompt`).
    server_name: String,
    /// Original prompt name as returned by `prompts/list`.
    prompt_name: String,
    /// Manager reference for calling `get_prompt`.
    manager: Arc<McpManager>,
}

impl McpPromptCommand {
    pub fn from_info(info: McpPromptInfo, manager: Arc<McpManager>) -> Self {
        Self {
            name: info.slash_path.clone(),
            description: info.description.clone(),
            args: info.arguments.clone(),
            server_name: info.server_name.clone(),
            prompt_name: info.prompt_name.clone(),
            manager,
        }
    }
}

impl SlashCommand for McpPromptCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if let Some(arg) = self.args.get(arg_index) {
            // Offer the argument name as a hint if partial is empty.
            if partial.is_empty() {
                return vec![CompletionItem {
                    value: format!("<{}>", arg.name),
                    display: format!("<{}>", arg.name),
                    description: if arg.description.is_empty() {
                        None
                    } else {
                        Some(arg.description.clone())
                    },
                    score: 0,
                }];
            }
        }
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        // Build named-argument map from positional args.
        let named: std::collections::HashMap<String, String> = self
            .args
            .iter()
            .zip(args.iter())
            .map(|(def, val)| (def.name.clone(), val.clone()))
            .collect();

        // We need to call get_prompt async but execute() is sync.
        // Use tokio::task::block_in_place to run it on the current thread.
        let result = {
            let mgr = Arc::clone(&self.manager);
            let server = self.server_name.clone();
            let prompt = self.prompt_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async move { mgr.get_prompt(&server, &prompt, named).await })
            })
        };

        match result {
            Ok(text) if !text.is_empty() => CommandResult {
                message_to_send: Some(text),
                ..Default::default()
            },
            Ok(_) => CommandResult::default(),
            Err(e) => CommandResult {
                message_to_send: Some(format!("Error fetching MCP prompt `{}`: {}", self.name, e)),
                ..Default::default()
            },
        }
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Query the `McpManager` for all available prompts and return them as
/// slash commands ready for registration.
///
/// Called by [`CommandRegistry::register_mcp_prompts`] after the manager
/// has connected to its configured servers.
pub async fn discover_mcp_prompts(manager: &Arc<McpManager>) -> Vec<McpPromptCommand> {
    let infos = manager.prompts().await;
    infos
        .into_iter()
        .map(|info| McpPromptCommand::from_info(info, Arc::clone(manager)))
        .collect()
}
