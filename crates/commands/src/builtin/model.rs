// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/model` command - override the model for the next queued message.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use sven_model::catalog;
use sven_model_drivers::credentials;

use super::model_menu;
use crate::{CommandContext, CommandResult, CompletionItem, SlashCommand};

/// Start a background listing for every source whose cache is missing or
/// stale. Building the provider is what starts it (hosted providers with a
/// key only; local servers are probed at start-up), and it needs a runtime.
fn refresh_stale_listings(sources: &[model_menu::Source]) {
    static REQUESTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    model_menu::request_stale_refreshes(
        sources,
        REQUESTED.get_or_init(Default::default),
        catalog::is_cache_stale,
        |config| {
            let _ = sven_model_drivers::from_config(config);
        },
    );
}

pub struct ModelCommand;

impl SlashCommand for ModelCommand {
    fn name(&self) -> &str {
        "model"
    }

    fn description(&self) -> &str {
        "Switch model permanently (e.g. /model anthropic/claude-opus-4-6)"
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
        let stored_keys = credentials::providers();
        let sources = model_menu::sources(ctx.providers, &ctx.current_model_provider, &stored_keys);
        for source_driver in sources.iter().map(model_menu::Source::driver_id) {
            catalog::load_disk_cache(source_driver);
        }
        refresh_stale_listings(&sources);
        let rows = model_menu::rows(
            &sources,
            &catalog::served_models(),
            (&ctx.current_model_provider, &ctx.current_model_name),
        );
        model_menu::items(rows, partial)
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
    fn execute_empty_arg_list_returns_no_override() {
        let result = ModelCommand.execute(vec![]);
        assert!(result.model_override.is_none());
    }

    #[test]
    fn execute_empty_string_arg_returns_no_override() {
        let result = ModelCommand.execute(vec!["".into()]);
        assert!(result.model_override.is_none());
    }
}
