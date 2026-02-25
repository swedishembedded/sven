// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/quit` command — exit the TUI.

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct QuitCommand;

impl SlashCommand for QuitCommand {
    fn name(&self) -> &str { "quit" }

    fn description(&self) -> &str { "Exit sven" }

    fn arguments(&self) -> Vec<CommandArgument> { vec![] }

    fn complete(&self, _arg_index: usize, _partial: &str, _ctx: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::Quit),
            ..Default::default()
        }
    }
}
