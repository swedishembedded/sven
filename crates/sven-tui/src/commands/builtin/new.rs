// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/new` command — start a completely new conversation with a fresh JSONL file.

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct NewCommand;

impl SlashCommand for NewCommand {
    fn name(&self) -> &str {
        "new"
    }

    fn description(&self) -> &str {
        "Start a completely new conversation with a fresh JSONL file"
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
            immediate_action: Some(ImmediateAction::NewConversation),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_returns_new_conversation_action() {
        let result = NewCommand.execute(vec![]);
        assert!(
            matches!(
                result.immediate_action,
                Some(ImmediateAction::NewConversation)
            ),
            "new must return ImmediateAction::NewConversation"
        );
    }

    #[test]
    fn execute_ignores_args() {
        let result = NewCommand.execute(vec!["confirm".into()]);
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::NewConversation)
        ));
    }

    #[test]
    fn execute_does_not_set_model_mode_or_message() {
        let result = NewCommand.execute(vec![]);
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }
}
