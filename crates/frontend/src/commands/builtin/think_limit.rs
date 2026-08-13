// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/think-limit` - inspect or live-adjust the thinking-loop watchdog.
//!
//! Guards against a model (local reasoning models such as Qwen are the
//! common case) that loops indefinitely in its reasoning instead of
//! converging to an answer. Two independent caps, whichever fires first:
//! total estimated thinking tokens for a turn, and wall-clock time spent
//! reasoning with no forward progress. See `sven_core::stream_turn` and the
//! `agent.max_thinking_tokens`/`agent.thinking_timeout_secs` config fields.
//!
//! Sets a process-wide override (`sven_core::set_thinking_budget_override`)
//! that takes effect starting with the very next turn - no session rebuild
//! needed, unlike `/model`. Applies to every concurrent session/subagent in
//! this process, which is the intended scope for "the model is looping, turn
//! this down right now".

use sven_core::ThinkingBudget;

use crate::commands::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand};

pub struct ThinkLimitCommand;

impl SlashCommand for ThinkLimitCommand {
    fn name(&self) -> &str {
        "think-limit"
    }

    fn description(&self) -> &str {
        "Show or set the thinking-loop watchdog (/think-limit [<max_tokens> [<timeout_secs>]] | off)"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }
        let candidates = [CompletionItem::with_desc(
            "off",
            "off",
            "clear the live override, revert to config/defaults",
        )];
        crate::commands::completion::filter_and_rank(candidates.to_vec(), partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        if args.is_empty() {
            return CommandResult {
                immediate_action: Some(ImmediateAction::ShareInstructions {
                    text: current_status_text(),
                }),
                ..Default::default()
            };
        }

        if matches!(args[0].as_str(), "off" | "reset" | "clear") {
            return CommandResult {
                immediate_action: Some(ImmediateAction::SetThinkingBudget(None)),
                ..Default::default()
            };
        }

        let max_thinking_tokens = args.first().and_then(|s| s.parse::<u32>().ok());
        if max_thinking_tokens.is_none() {
            return CommandResult {
                immediate_action: Some(ImmediateAction::ShareInstructions {
                    text: "Usage: /think-limit [<max_tokens> [<timeout_secs>]] | off".to_string(),
                }),
                ..Default::default()
            };
        }
        let thinking_timeout_secs = args.get(1).and_then(|s| s.parse::<u64>().ok());

        CommandResult {
            immediate_action: Some(ImmediateAction::SetThinkingBudget(Some(ThinkingBudget {
                max_thinking_tokens,
                thinking_timeout_secs,
            }))),
            ..Default::default()
        }
    }
}

/// Render the currently active watchdog state as a local notice (never sent
/// to the model - see `ImmediateAction::ShareInstructions`'s doc comment).
fn current_status_text() -> String {
    match sven_core::thinking_budget_override() {
        Some(b) => format!(
            "Thinking-loop watchdog: live override active for this process\n  \
             max_thinking_tokens: {}\n  thinking_timeout_secs: {}\n\n\
             /think-limit off  - clear the override, revert to config/defaults",
            b.max_thinking_tokens
                .map_or("default (10% of context window)".to_string(), |v| v.to_string()),
            b.thinking_timeout_secs
                .map_or("default (600)".to_string(), |v| v.to_string()),
        ),
        None => "Thinking-loop watchdog: no live override\n\
                 Using agent.max_thinking_tokens / agent.thinking_timeout_secs from config\n\
                 (or the built-in defaults: 10% of context window / 600s).\n\n\
                 /think-limit <max_tokens> [<timeout_secs>]  - set a live override\n\
                 /think-limit off                            - clear an override"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_shows_status_without_mutating_anything() {
        // Reading status must be a pure query - no SetThinkingBudget action.
        let result = ThinkLimitCommand.execute(vec![]);
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::ShareInstructions { .. })
        ));
    }

    #[test]
    fn off_clears_the_override() {
        let result = ThinkLimitCommand.execute(vec!["off".into()]);
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::SetThinkingBudget(None))
        ));
    }

    #[test]
    fn reset_and_clear_are_aliases_for_off() {
        for alias in ["reset", "clear"] {
            let result = ThinkLimitCommand.execute(vec![alias.into()]);
            assert!(
                matches!(result.immediate_action, Some(ImmediateAction::SetThinkingBudget(None))),
                "{alias} must behave like off"
            );
        }
    }

    #[test]
    fn tokens_only_sets_partial_override() {
        let result = ThinkLimitCommand.execute(vec!["20000".into()]);
        match result.immediate_action {
            Some(ImmediateAction::SetThinkingBudget(Some(b))) => {
                assert_eq!(b.max_thinking_tokens, Some(20_000));
                assert_eq!(b.thinking_timeout_secs, None);
            }
            other => panic!("expected SetThinkingBudget(Some(..)), got {other:?}"),
        }
    }

    #[test]
    fn tokens_and_seconds_sets_full_override() {
        let result = ThinkLimitCommand.execute(vec!["20000".into(), "300".into()]);
        match result.immediate_action {
            Some(ImmediateAction::SetThinkingBudget(Some(b))) => {
                assert_eq!(b.max_thinking_tokens, Some(20_000));
                assert_eq!(b.thinking_timeout_secs, Some(300));
            }
            other => panic!("expected SetThinkingBudget(Some(..)), got {other:?}"),
        }
    }

    #[test]
    fn unparseable_first_arg_shows_usage_without_mutating_anything() {
        let result = ThinkLimitCommand.execute(vec!["not-a-number".into()]);
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::ShareInstructions { .. })
        ));
    }
}
