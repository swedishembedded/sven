// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/abort` — abort the current model run.

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

/// Abort the current model run.
///
/// If the agent is currently running, the active turn is cancelled.  Any text
/// that was already streamed is preserved as a partial assistant message.
///
/// If there are queued messages, they are **not** automatically submitted after
/// the abort; the user must submit the next message manually by selecting it in
/// the queue panel and pressing `s` (or by typing a new message, which will
/// also be queued until explicitly sent).
///
/// To immediately send a queued message instead of just stopping, use the
/// force-submit binding (`f`) in the queue panel.
pub struct AbortCommand;

impl SlashCommand for AbortCommand {
    fn name(&self) -> &str {
        "abort"
    }

    fn description(&self) -> &str {
        "Abort the current model run (preserves partial output; queued messages stay queued)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![]
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
