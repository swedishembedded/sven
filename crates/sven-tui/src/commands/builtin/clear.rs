// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/clear` command — erase all chat segments and reset the conversation view.

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct ClearCommand;

impl SlashCommand for ClearCommand {
    fn name(&self) -> &str { "clear" }

    fn description(&self) -> &str { "Clear the chat history" }

    fn complete(&self, _arg_index: usize, _partial: &str, _ctx: &CommandContext) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::ClearChat),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_returns_clear_chat_action() {
        let result = ClearCommand.execute(vec![]);
        assert!(
            matches!(result.immediate_action, Some(ImmediateAction::ClearChat)),
            "clear must return ImmediateAction::ClearChat"
        );
    }

    #[test]
    fn execute_ignores_args() {
        let result = ClearCommand.execute(vec!["all".into()]);
        assert!(matches!(result.immediate_action, Some(ImmediateAction::ClearChat)));
    }

    #[test]
    fn execute_does_not_set_model_mode_or_message() {
        let result = ClearCommand.execute(vec![]);
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }
}
