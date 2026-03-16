// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Inspector slash commands: `/skills`, `/subagents`, `/peers`, `/context`, `/tools`, `/mcp`.

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, InspectorKind, SlashCommand,
};

// ── /skills ───────────────────────────────────────────────────────────────────

pub struct SkillsCommand;

impl SlashCommand for SkillsCommand {
    fn name(&self) -> &str {
        "skills"
    }

    fn description(&self) -> &str {
        "Show all available skills as a browsable tree with paths and metadata."
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Skills,
            }),
            ..Default::default()
        }
    }
}

// ── /subagents ────────────────────────────────────────────────────────────────

pub struct SubagentsCommand;

impl SlashCommand for SubagentsCommand {
    fn name(&self) -> &str {
        "subagents"
    }

    fn description(&self) -> &str {
        "Show all configured subagents with their descriptions, models, and paths."
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Subagents,
            }),
            ..Default::default()
        }
    }
}

// ── /peers ────────────────────────────────────────────────────────────────────

pub struct PeersCommand;

impl SlashCommand for PeersCommand {
    fn name(&self) -> &str {
        "peers"
    }

    fn description(&self) -> &str {
        "Show configured subagents and active subprocess buffers."
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Peers,
            }),
            ..Default::default()
        }
    }
}

// ── /context ──────────────────────────────────────────────────────────────────

pub struct ContextCommand;

impl SlashCommand for ContextCommand {
    fn name(&self) -> &str {
        "context"
    }

    fn description(&self) -> &str {
        "Show the current agent context: project root, skills/agents counts, \
         and active output buffer handles."
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Context,
            }),
            ..Default::default()
        }
    }
}

// ── /tools ────────────────────────────────────────────────────────────────────

pub struct ToolsCommand;

impl SlashCommand for ToolsCommand {
    fn name(&self) -> &str {
        "tools"
    }

    fn description(&self) -> &str {
        "Show all available tools with descriptions and parameter counts."
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Tools,
            }),
            ..Default::default()
        }
    }
}

// ── /mcp ──────────────────────────────────────────────────────────────────────

pub struct McpCommand;

impl SlashCommand for McpCommand {
    fn name(&self) -> &str {
        "mcp"
    }

    fn description(&self) -> &str {
        "Show and manage MCP servers. Usage: /mcp [list|enable|disable|auth] [name]"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index == 0 {
            ["list", "enable", "disable", "auth"]
                .iter()
                .filter(|s| s.starts_with(partial))
                .map(|s| CompletionItem {
                    value: s.to_string(),
                    display: s.to_string(),
                    description: None,
                    score: 0,
                })
                .collect()
        } else if arg_index == 1 {
            let servers: Vec<&str> = ctx.config.mcp_servers.keys().map(String::as_str).collect();
            servers
                .into_iter()
                .filter(|s| s.starts_with(partial))
                .map(|s| CompletionItem {
                    value: s.to_string(),
                    display: s.to_string(),
                    description: None,
                    score: 0,
                })
                .collect()
        } else {
            vec![]
        }
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let sub = args.first().map(String::as_str).unwrap_or("");
        let server = args.get(1).cloned().unwrap_or_default();

        match (sub, server.as_str()) {
            ("auth", name) if !name.is_empty() => CommandResult {
                immediate_action: Some(ImmediateAction::McpAuth {
                    server: name.to_string(),
                }),
                ..Default::default()
            },
            _ => CommandResult {
                immediate_action: Some(ImmediateAction::OpenInspector {
                    kind: InspectorKind::Mcp,
                }),
                ..Default::default()
            },
        }
    }
}
