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
