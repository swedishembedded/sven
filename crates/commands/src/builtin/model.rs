// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/model` command - override the model for the next queued message.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use sven_model::catalog;

use super::model_menu;
use crate::{CommandContext, CommandResult, CompletionItem, SlashCommand};

/// Ask every source to list its models again in the background, at most once
/// a minute each (`model_menu::request_refreshes`); it needs a runtime.
fn refresh_listings(sources: &[model_menu::Source]) {
    static REQUESTED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    model_menu::request_refreshes(
        sources,
        REQUESTED.get_or_init(Default::default),
        Instant::now(),
        sven_model_drivers::listing::refresh,
    );
}

/// Why `cfg` cannot be switched to, when its provider's model list (the
/// cache `/model` keeps current) is known and does not have it: a model no
/// endpoint lists could only fail at the first request. `None` when it is
/// listed, or when nothing is known about its provider yet.
#[must_use]
pub fn unavailable_model(cfg: &sven_model_drivers::ModelConfig) -> Option<String> {
    catalog::load_disk_cache(&cfg.provider);
    let served = catalog::served_models();
    let mut listed = served
        .iter()
        .filter(|e| e.provider == cfg.provider)
        .peekable();
    listed.peek()?;
    if listed.any(|e| e.id == cfg.name) {
        return None;
    }
    Some(format!(
        "{}/{} is not among the models {} serves now; /model lists them",
        cfg.provider, cfg.name, cfg.provider
    ))
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
        let stored_keys = sven_model_drivers::sven_service::keyed_providers();
        let sources = model_menu::sources(ctx.providers, &ctx.current_model_provider, &stored_keys);
        for source_driver in sources.iter().map(model_menu::Source::driver_id) {
            catalog::load_disk_cache(source_driver);
        }
        refresh_listings(&sources);
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
    fn a_model_its_provider_does_not_list_is_unavailable() {
        catalog::cache_seed([sven_model::catalog::ModelCatalogEntry::bare(
            "listed-provider",
            "served",
        )]);
        let cfg = |provider: &str, name: &str| sven_model_drivers::ModelConfig {
            provider: provider.into(),
            name: name.into(),
            ..Default::default()
        };
        assert_eq!(unavailable_model(&cfg("listed-provider", "served")), None);
        assert!(unavailable_model(&cfg("listed-provider", "gone"))
            .is_some_and(|why| why.contains("listed-provider/gone")));
        // Nothing known about a provider is no reason to refuse it.
        assert_eq!(unavailable_model(&cfg("never-listed", "x")), None);
    }

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
