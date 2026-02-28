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
    fn name(&self) -> &str {
        "model"
    }

    fn description(&self) -> &str {
        "Switch model permanently (e.g. /model anthropic/claude-opus-4-6)"
    }

    fn arguments(&self) -> Vec<CommandArgument> {
        vec![CommandArgument::required(
            "model",
            "Model specifier: provider/name, named provider, or bare model name from catalog",
        )]
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

        let current_value = format!("{}/{}", ctx.current_model_provider, ctx.current_model_name);
        let current_item = CompletionItem::with_desc(
            current_value.clone(),
            format!("{} (current)", current_value),
            "currently active model",
        );

        // Build the candidate list (excluding the current model to avoid duplicates).
        let mut candidates: Vec<CompletionItem> = Vec::new();

        // 1. Named custom providers from config.providers.
        let mut provider_names: Vec<&str> =
            ctx.config.providers.keys().map(|s| s.as_str()).collect();
        provider_names.sort_unstable();
        for name in provider_names {
            let cfg = &ctx.config.providers[name];
            let value = format!("{}/{}", cfg.provider, cfg.name);
            if value == current_value {
                continue;
            }
            let display = format!("{} (custom: {}  {})", name, cfg.provider, cfg.name);
            candidates.push(CompletionItem::with_desc(
                name,
                display,
                "custom provider from config",
            ));
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
            let desc = format!(
                "ctx:{} max_out:{}",
                entry.context_window, entry.max_output_tokens
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_bare_model_name_sets_override() {
        let result = ModelCommand.execute(vec!["gpt-4o".into()]);
        assert_eq!(result.model_override.as_deref(), Some("gpt-4o"));
        assert!(result.mode_override.is_none());
        assert!(result.immediate_action.is_none());
    }

    #[test]
    fn execute_provider_slash_model_sets_override() {
        let result = ModelCommand.execute(vec!["anthropic/claude-opus-4-6".into()]);
        assert_eq!(
            result.model_override.as_deref(),
            Some("anthropic/claude-opus-4-6")
        );
    }

    #[test]
    fn execute_openai_catalog_model_sets_override() {
        let result = ModelCommand.execute(vec!["openai/gpt-4o".into()]);
        assert_eq!(result.model_override.as_deref(), Some("openai/gpt-4o"));
    }

    #[test]
    fn execute_named_custom_provider_sets_override() {
        let result = ModelCommand.execute(vec!["my_ollama".into()]);
        assert_eq!(result.model_override.as_deref(), Some("my_ollama"));
    }

    #[test]
    fn execute_empty_arg_list_returns_no_override() {
        let result = ModelCommand.execute(vec![]);
        assert!(result.model_override.is_none(), "no override when no args");
    }

    #[test]
    fn execute_empty_string_arg_returns_no_override() {
        // Happens when user presses Enter after "/model " with nothing typed.
        let result = ModelCommand.execute(vec!["".into()]);
        assert!(
            result.model_override.is_none(),
            "empty string must not set override"
        );
    }

    #[test]
    fn execute_does_not_set_mode_or_immediate_action() {
        let result = ModelCommand.execute(vec!["gpt-4o".into()]);
        assert!(result.mode_override.is_none());
        assert!(result.immediate_action.is_none());
        assert!(result.message_to_send.is_none());
    }
}
