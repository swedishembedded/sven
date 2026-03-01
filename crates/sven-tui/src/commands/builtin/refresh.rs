// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/refresh` command — re-scan skill directories and rebuild slash commands.

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct RefreshCommand;

impl SlashCommand for RefreshCommand {
    fn name(&self) -> &str {
        "refresh"
    }

    fn description(&self) -> &str {
        "Re-scan skill directories and update slash commands"
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
            immediate_action: Some(ImmediateAction::RefreshSkills),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_returns_refresh_skills_action() {
        let result = RefreshCommand.execute(vec![]);
        assert!(
            matches!(
                result.immediate_action,
                Some(ImmediateAction::RefreshSkills)
            ),
            "refresh must return ImmediateAction::RefreshSkills"
        );
    }

    #[test]
    fn execute_does_not_set_model_mode_or_message() {
        let result = RefreshCommand.execute(vec![]);
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }
}
