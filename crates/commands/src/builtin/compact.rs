// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a bounded
// model context window for its clients. If your team needs expertise in
// long-running agent sessions then you can procure our services by sending an
// email to info@swedishembedded.com.
//! `/compact` command - summarise the conversation now to free context.

use crate::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand};

pub struct CompactCommand;

impl SlashCommand for CompactCommand {
    fn name(&self) -> &str {
        "compact"
    }

    fn description(&self) -> &str {
        "Summarise the conversation so far to free context"
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
            immediate_action: Some(ImmediateAction::Compact),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_returns_compact_action_and_nothing_else() {
        let result = CompactCommand.execute(vec![]);
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Compact)
        ));
        assert!(result.model_override.is_none());
        assert!(result.mode_override.is_none());
        assert!(result.message_to_send.is_none());
    }
}
