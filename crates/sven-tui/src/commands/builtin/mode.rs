// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/mode` command — switch the agent mode for the next queued message.

use sven_config::AgentMode;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

pub struct ModeCommand;

/// All supported mode names with their descriptions.
static MODES: &[(&str, &str)] = &[
    ("research", "Read-only tools — explores and answers, no writes"),
    ("plan", "Generates a structured plan without making code changes"),
    ("agent", "Full agent with read/write tools (default)"),
];

impl SlashCommand for ModeCommand {
    fn name(&self) -> &str { "mode" }

    fn description(&self) -> &str {
        "Switch agent mode for the next message (research / plan / agent)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::required(
            "mode",
            "Agent mode: research, plan, or agent",
        )]
    }

    fn complete(&self, arg_index: usize, partial: &str, _ctx: &CommandContext) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }

        let items: Vec<CompletionItem> = MODES
            .iter()
            .map(|(name, desc)| CompletionItem::with_desc(*name, *name, *desc))
            .collect();

        crate::commands::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let mode_str = args.into_iter().next().unwrap_or_default();
        let mode = match mode_str.as_str() {
            "research" => Some(AgentMode::Research),
            "plan"     => Some(AgentMode::Plan),
            "agent"    => Some(AgentMode::Agent),
            _          => None,
        };
        CommandResult {
            mode_override: mode,
            ..Default::default()
        }
    }
}
