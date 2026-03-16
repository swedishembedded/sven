// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/refresh` command — re-scan skill directories and rebuild slash commands.

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct RefreshCommand;

impl SlashCommand for RefreshCommand {
    fn name(&self) -> &str {
        "refresh"
    }

    fn description(&self) -> &str {
        "Re-scan skill directories and update slash commands"
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::RefreshSkills),
            ..Default::default()
        }
    }
}
