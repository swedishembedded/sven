// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/provider` command — switch provider while keeping current model name.

use sven_model::registry;

use crate::commands::{CommandContext, CommandResult, CompletionItem, SlashCommand};

pub struct ProviderCommand;

impl SlashCommand for ProviderCommand {
    fn name(&self) -> &str {
        "provider"
    }

    fn description(&self) -> &str {
        "Switch provider for the next message (keeps current model name)"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        let mut provider_names: Vec<&str> =
            ctx.config.providers.keys().map(|s| s.as_str()).collect();
        provider_names.sort_unstable();
        for name in provider_names {
            let cfg = &ctx.config.providers[name];
            let model_count = cfg.models.len();
            let display = format!("{} (driver: {}  models: {})", name, cfg.name, model_count);
            items.push(CompletionItem::with_desc(
                name,
                display,
                "custom provider from config",
            ));
        }

        for driver in registry::list_drivers() {
            let display = format!("{} — {}", driver.id, driver.name);
            let desc = driver.description.to_string();
            items.push(CompletionItem::with_desc(driver.id, display, desc));
        }

        crate::commands::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let provider = args.into_iter().next().unwrap_or_default();
        if provider.is_empty() {
            return CommandResult::default();
        }
        CommandResult {
            model_override: Some(provider),
            ..Default::default()
        }
    }
}
