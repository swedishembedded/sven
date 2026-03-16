// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP-prompt-based slash commands.

use std::sync::Arc;

use sven_mcp_client::{McpManager, McpPromptArgInfo, McpPromptInfo};

use crate::commands::{CommandContext, CommandResult, CompletionItem, SlashCommand};

// ── McpPromptCommand ──────────────────────────────────────────────────────────

pub struct McpPromptCommand {
    name: String,
    description: String,
    args: Vec<McpPromptArgInfo>,
    server_name: String,
    prompt_name: String,
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
        let named: std::collections::HashMap<String, String> = self
            .args
            .iter()
            .zip(args.iter())
            .map(|(def, val)| (def.name.clone(), val.clone()))
            .collect();

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

pub async fn discover_mcp_prompts(manager: &Arc<McpManager>) -> Vec<McpPromptCommand> {
    let infos = manager.prompts().await;
    infos
        .into_iter()
        .map(|info| McpPromptCommand::from_info(info, Arc::clone(manager)))
        .collect()
}
