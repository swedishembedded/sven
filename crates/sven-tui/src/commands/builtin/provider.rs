// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/provider` command — switch provider for the next queued message.
//!
//! When only a provider is specified the current model name is kept but the
//! provider and its default base_url are used.  This is a convenience
//! shorthand for `/model provider/current-model`.

use sven_model::registry;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

pub struct ProviderCommand;

impl SlashCommand for ProviderCommand {
    fn name(&self) -> &str { "provider" }

    fn description(&self) -> &str {
        "Switch provider for the next message (keeps current model name)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::required(
            "provider",
            "Provider id (e.g. openai, anthropic, ollama) or named provider from config",
        )]
    }

    fn complete(&self, arg_index: usize, partial: &str, ctx: &CommandContext) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        // Named custom providers from config.providers first
        let mut provider_names: Vec<&str> = ctx.config.providers.keys().map(|s| s.as_str()).collect();
        provider_names.sort_unstable();
        for name in provider_names {
            let cfg = &ctx.config.providers[name];
            let display = format!("{} (custom: {}  {})", name, cfg.provider, cfg.name);
            items.push(CompletionItem::with_desc(name, display, "custom provider from config"));
        }

        // Built-in provider drivers
        for driver in registry::list_drivers() {
            let display = format!("{} — {}", driver.id, driver.name);
            let desc = format!("{}", driver.description);
            items.push(CompletionItem::with_desc(driver.id, display, desc));
        }

        crate::commands::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let provider = args.into_iter().next().unwrap_or_default();
        if provider.is_empty() {
            return CommandResult::default();
        }
        // Passing just the provider id to the model resolution layer changes
        // the provider while keeping the current model name.
        CommandResult {
            model_override: Some(provider),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_known_provider_sets_model_override() {
        // The provider id is forwarded as a model_override; resolve_model_from_config
        // recognises bare provider ids and keeps the current model name.
        let result = ProviderCommand.execute(vec!["anthropic".into()]);
        assert_eq!(result.model_override.as_deref(), Some("anthropic"));
        assert!(result.mode_override.is_none());
        assert!(result.immediate_action.is_none());
    }

    #[test]
    fn execute_openai_provider_sets_model_override() {
        let result = ProviderCommand.execute(vec!["openai".into()]);
        assert_eq!(result.model_override.as_deref(), Some("openai"));
    }

    #[test]
    fn execute_named_custom_provider_sets_model_override() {
        let result = ProviderCommand.execute(vec!["my_ollama".into()]);
        assert_eq!(result.model_override.as_deref(), Some("my_ollama"));
    }

    #[test]
    fn execute_empty_args_returns_no_override() {
        let result = ProviderCommand.execute(vec![]);
        assert!(result.model_override.is_none());
    }

    #[test]
    fn execute_empty_string_returns_no_override() {
        let result = ProviderCommand.execute(vec!["".into()]);
        assert!(result.model_override.is_none());
    }
}
