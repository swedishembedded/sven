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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_returns_quit_immediate_action() {
        let result = QuitCommand.execute(vec![]);
        assert!(
            matches!(result.immediate_action, Some(ImmediateAction::Quit)),
            "quit must return ImmediateAction::Quit"
        );
    }

    #[test]
    fn execute_ignores_unexpected_args() {
        // /quit should quit regardless of any trailing arguments.
        let result = QuitCommand.execute(vec!["now".into(), "please".into()]);
        assert!(matches!(result.immediate_action, Some(ImmediateAction::Quit)));
    }

    #[test]
    fn execute_does_not_set_model_or_mode_override() {
        let result = QuitCommand.execute(vec![]);
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }

    #[test]
    fn complete_always_returns_empty() {
        use crate::commands::CommandContext;
        use std::sync::Arc;
        use sven_config::Config;
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        assert!(QuitCommand.complete(0, "", &ctx).is_empty());
        assert!(QuitCommand.complete(0, "any", &ctx).is_empty());
    }
}
