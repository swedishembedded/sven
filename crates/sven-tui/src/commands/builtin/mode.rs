// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/mode` command — switch the agent mode for the next queued message.

use sven_config::AgentMode;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

pub struct ModeCommand;

/// All supported mode names with their descriptions.
static MODES: &[(&str, &str)] = &[
    (
        "research",
        "Read-only tools — explores and answers, no writes",
    ),
    (
        "plan",
        "Generates a structured plan without making code changes",
    ),
    ("agent", "Full agent with read/write tools (default)"),
];

impl SlashCommand for ModeCommand {
    fn name(&self) -> &str {
        "mode"
    }

    fn description(&self) -> &str {
        "Switch agent mode for the next message (research / plan / agent)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::required(
            "mode",
            "Agent mode: research, plan, or agent",
        )]
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

        let items: Vec<CompletionItem> = MODES
            .iter()
            .map(|(name, desc)| CompletionItem::with_desc(*name, *name, *desc))
            .collect();

        crate::commands::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let mode_str = args.into_iter().next().unwrap_or_default();
        let mode = match mode_str.as_str() {
            "research" => Some(AgentMode::Research),
            "plan" => Some(AgentMode::Plan),
            "agent" => Some(AgentMode::Agent),
            _ => None,
        };
        CommandResult {
            mode_override: mode,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_research_sets_research_mode() {
        let result = ModeCommand.execute(vec!["research".into()]);
        assert_eq!(result.mode_override, Some(AgentMode::Research));
    }

    #[test]
    fn execute_plan_sets_plan_mode() {
        let result = ModeCommand.execute(vec!["plan".into()]);
        assert_eq!(result.mode_override, Some(AgentMode::Plan));
    }

    #[test]
    fn execute_agent_sets_agent_mode() {
        let result = ModeCommand.execute(vec!["agent".into()]);
        assert_eq!(result.mode_override, Some(AgentMode::Agent));
    }

    #[test]
    fn execute_unknown_mode_returns_no_override() {
        // An unrecognised mode name must not silently set any override.
        let result = ModeCommand.execute(vec!["invalid".into()]);
        assert!(
            result.mode_override.is_none(),
            "unknown mode must not set override"
        );
    }

    #[test]
    fn execute_empty_args_returns_no_override() {
        let result = ModeCommand.execute(vec![]);
        assert!(result.mode_override.is_none());
    }

    #[test]
    fn execute_does_not_set_model_or_immediate_action() {
        let result = ModeCommand.execute(vec!["plan".into()]);
        assert!(result.model_override.is_none());
        assert!(result.immediate_action.is_none());
        assert!(result.message_to_send.is_none());
    }

    #[test]
    fn complete_returns_all_three_modes_when_filter_is_empty() {
        use crate::commands::CommandContext;
        use std::sync::Arc;
        use sven_config::Config;
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        let items = ModeCommand.complete(0, "", &ctx);
        let names: Vec<&str> = items.iter().map(|i| i.value.as_str()).collect();
        assert!(names.contains(&"research"), "research must be listed");
        assert!(names.contains(&"plan"), "plan must be listed");
        assert!(names.contains(&"agent"), "agent must be listed");
    }

    #[test]
    fn complete_filters_by_prefix() {
        use crate::commands::CommandContext;
        use std::sync::Arc;
        use sven_config::Config;
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        let items = ModeCommand.complete(0, "res", &ctx);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].value, "research");
    }

    #[test]
    fn complete_wrong_arg_index_returns_empty() {
        use crate::commands::CommandContext;
        use std::sync::Arc;
        use sven_config::Config;
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        assert!(ModeCommand.complete(1, "", &ctx).is_empty());
    }
}
