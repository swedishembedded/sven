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

        let current_value = format!("{}/{}", ctx.current_model_provider, ctx.current_model_name);
        let current_item = CompletionItem::with_desc(
            current_value.clone(),
            format!("{} (current)", current_value),
            "currently active model",
        );

        // Build the candidate list (excluding the current model to avoid duplicates).
        let mut candidates: Vec<CompletionItem> = Vec::new();

        // 1. Named custom providers from config.providers.
        let mut provider_names: Vec<&str> = ctx.config.providers.keys().map(|s| s.as_str()).collect();
        provider_names.sort_unstable();
        for name in provider_names {
            let cfg = &ctx.config.providers[name];
            let value = format!("{}/{}", cfg.provider, cfg.name);
            if value == current_value {
                continue;
            }
            let display = format!("{} (custom: {}  {})", name, cfg.provider, cfg.name);
            candidates.push(CompletionItem::with_desc(name, display, "custom provider from config"));
        }

        // 2. All models from static catalog in provider/id form.
        let mut catalog_models = catalog::static_catalog();
        catalog_models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));

        for entry in catalog_models {
            let value = format!("{}/{}", entry.provider, entry.id);
            if value == current_value {
                continue;
            }
            let display = if entry.description.is_empty() {
                value.clone()
            } else {
                format!("{} — {}", value, entry.description)
            };
            let desc = format!("ctx:{} max_out:{}", entry.context_window, entry.max_output_tokens);
            candidates.push(CompletionItem::with_desc(value, display, desc));
        }

        // Fuzzy-filter the candidates.
        let mut ranked = crate::commands::completion::filter_and_rank(candidates, partial);

        // Prepend the current model if it matches the filter (or if there is no
        // filter yet).  This guarantees it is always visible as the first entry,
        // making it easy for the user to see exactly which model is active.
        let current_matches = partial.is_empty()
            || crate::commands::completion::fuzzy_score(partial, &current_value).is_some();
        if current_matches {
            ranked.insert(0, current_item);
        }

        ranked
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
