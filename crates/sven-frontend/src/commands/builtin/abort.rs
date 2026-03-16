// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/abort` — abort the current model run.

use crate::commands::{CommandContext, CommandResult, CompletionItem, SlashCommand};

pub struct AbortCommand;

impl SlashCommand for AbortCommand {
    fn name(&self) -> &str {
        "abort"
    }

    fn description(&self) -> &str {
        "Abort the current model run (preserves partial output; queued messages stay queued)"
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(crate::commands::ImmediateAction::Abort),
            ..Default::default()
        }
    }
}
