// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `/model` command — override the model for the next queued message.
//!
//! Completions include:
//! - Custom named providers from `config.providers` (e.g. `my_ollama`)
//! - All models from the static catalog in `provider/model-id` form
//!
//! Smart resolution: when a bare model name matches the catalog (e.g. `gpt-4o`)
//! the provider and its default base_url are resolved automatically by
//! `sven_model::resolve_model_from_config`.

use sven_model::catalog;

use crate::commands::{
    CommandArgument, CommandContext, CommandResult, CompletionItem, SlashCommand,
};

pub struct ModelCommand;

impl SlashCommand for ModelCommand {
    fn name(&self) -> &str { "model" }

    fn description(&self) -> &str {
        "Switch model for the next message (e.g. /model anthropic/claude-opus-4-6)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::required(
            "model",
            "Model specifier: provider/name, named provider, or bare model name from catalog",
        )]
    }

    fn complete(&self, arg_index: usize, partial: &str, ctx: &CommandContext) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        // 1. Named custom providers from config.providers
        let mut provider_names: Vec<&str> = ctx.config.providers.keys().map(|s| s.as_str()).collect();
        provider_names.sort_unstable();
        for name in provider_names {
            let cfg = &ctx.config.providers[name];
            let display = format!("{} (custom: {}  {})", name, cfg.provider, cfg.name);
            items.push(CompletionItem::with_desc(name, display, "custom provider from config"));
        }

        // 2. All models from static catalog in provider/id form
        let mut catalog_models = catalog::static_catalog();
        catalog_models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));

        for entry in catalog_models {
            let value = format!("{}/{}", entry.provider, entry.id);
            let display = if entry.description.is_empty() {
                format!("{}/{}", entry.provider, entry.id)
            } else {
                format!("{}/{} — {}", entry.provider, entry.id, entry.description)
            };
            let desc = format!("ctx:{} max_out:{}", entry.context_window, entry.max_output_tokens);
            items.push(CompletionItem::with_desc(value, display, desc));
        }

        // Filter based on partial input
        crate::commands::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let model = args.into_iter().next().unwrap_or_default();
        if model.is_empty() {
            return CommandResult::default();
        }
        CommandResult {
            model_override: Some(model),
            ..Default::default()
        }
    }
}
