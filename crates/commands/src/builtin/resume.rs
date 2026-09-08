// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/resume` command - open the session picker to reload a saved conversation.

use crate::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand};

pub struct ResumeCommand;

impl SlashCommand for ResumeCommand {
    fn name(&self) -> &str {
        "resume"
    }

    fn description(&self) -> &str {
        "Browse and reload a saved conversation"
    }

    fn complete(&self, _: usize, _: &str, _: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenSessionPicker),
            ..Default::default()
        }
    }
}
