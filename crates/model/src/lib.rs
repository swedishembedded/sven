// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel-tier vocabulary every LLM-facing crate in the workspace
//! shares: the [`ModelProvider`] trait, the request/response types
//! (`CompletionRequest`, `ResponseEvent`, `Message`, ...), the pure
//! prompt-size budget gate (`budget`), image-support sanitisation
//! (`sanitize`), the static driver registry (`registry`), and the
//! `ModelResolver`/`resolve_model_cfg` config-resolution logic.
//!
//! Deliberately **not** here (refactor plan Phase 5 — this crate used to
//! bundle all of it as one ~10k-LOC god crate): the concrete driver
//! implementations and the `from_config`/`from_config_probed` factory that
//! constructs them (now [`sven_model_drivers`], which owns `reqwest` and
//! every provider's own dependency closure), the static catalog data (now
//! [`sven_model_catalog`], re-exported here at [`catalog`] for source
//! compatibility), and the `--model mock` test/dev providers (now
//! [`sven_model_mock`]). `ModelResolver`/`resolve_model_cfg` stay here
//! rather than moving to the drivers crate — on inspection they never
//! construct a driver, only a [`sven_config::ModelConfig`] (see
//! [`ModelResolver::resolve`]), so they have no reqwest dependency and
//! belong with the rest of the pure vocabulary.
pub mod budget;
/// Re-exported from the standalone [`sven_model_catalog`] crate, kept at
/// this path (`sven_model::catalog::*`) so the ~20 existing call sites
/// across the workspace (`sven_model::catalog::static_catalog()`,
/// `sven_model::InputModality`, ...) do not all need to change in the same
/// commit that pulls the catalog data out into its own kernel-tier crate.
/// The catalog itself (`ModelCatalogEntry`, `static_catalog()`, `lookup()`,
/// the on-disk live-cache overlay) now lives in `sven-model-catalog`, which
/// has zero heavy dependencies (no reqwest, no cloud SDKs) — exactly like
/// `sven-model` itself post-split, so re-exporting it here does not undo
/// the point of the split.
pub use sven_model_catalog as catalog;
#[cfg(all(unix, feature = "dbus"))]
pub mod dbus;
mod provider;
pub mod registry;
pub mod sanitize;
mod types;

pub use catalog::{InputModality, ModelCatalogEntry};
#[cfg(all(unix, feature = "dbus"))]
pub use dbus::action::{ActionClient, ActionInput, ActionOutcome};
pub use dbus::{BusKind, DbusOptions, DbusProvider};
pub use provider::{ModelProvider, ResponseStream};
pub use registry::{get_driver, list_drivers, DriverMeta};
pub use types::*;

use sven_config::ModelConfig;

// ── ModelResolver ─────────────────────────────────────────────────────────────

/// Resolves a user-supplied model string to a [`ModelConfig`].
///
/// Resolution happens in four ordered steps; the first one that succeeds wins:
///
/// 1. **Named provider** - if the prefix of `override_str` matches a key in
///    `config.providers`, use that named config (optionally overriding the
///    model name with the suffix after `/`).
/// 2. **Catalog lookup `provider/name`** - when `override_str` contains `/`
///    and the prefix is a known driver, look up `(provider, name)` in the
///    static model catalog.  A fresh `ModelConfig` is built from catalog
///    metadata; credentials are inherited only when the resolved provider
///    matches `config.model.provider`.
/// 3. **Catalog lookup by bare model name** - when `override_str` has no `/`
///    and is not a known provider id, search the catalog for that model name
///    alone (provider is inferred from the catalog entry).
/// 4. **Fallback** - call [`resolve_model_cfg`] with `config.model` as the
///    base, which handles bare provider ids and custom/unknown endpoints.
pub struct ModelResolver<'a> {
    config: &'a sven_config::Config,
    override_str: &'a str,
}

impl<'a> ModelResolver<'a> {
    pub fn new(config: &'a sven_config::Config, override_str: &'a str) -> Self {
        Self {
            config,
            override_str,
        }
    }

    /// Run all four resolution steps in priority order.
    pub fn resolve(self) -> ModelConfig {
        let (provider_key, model_suffix) = self.parse_override();
        if let Some(cfg) = self.try_named_provider(provider_key, model_suffix) {
            return cfg;
        }
        if let Some(cfg) = self.try_catalog_by_provider_name(provider_key, model_suffix) {
            return cfg;
        }
        if let Some(cfg) = self.try_catalog_by_bare_model_name(provider_key, model_suffix) {
            return cfg;
        }
        self.fallback()
    }

    /// Step 0 (pre-processing): split `override_str` at the first `/`.
    fn parse_override(&self) -> (&str, Option<&str>) {
        if let Some((p, m)) = self.override_str.split_once('/') {
            (p, Some(m))
        } else {
            (self.override_str, None)
        }
    }

    /// Step 1: check `config.providers` for a named custom provider.
    fn try_named_provider(
        &self,
        provider_key: &str,
        model_suffix: Option<&str>,
    ) -> Option<ModelConfig> {
        let entry = self.config.providers.get(provider_key)?;
        // When no model suffix is given, keep the current model name from the
        // active config so that `--model my_ollama` switches the provider endpoint
        // without changing the model name.
        let model_name = model_suffix.unwrap_or(&self.config.model.name);
        Some(entry.to_model_config(model_name))
    }

    /// Step 2: catalog lookup by `provider/name` when the provider is a
    /// known driver.
    fn try_catalog_by_provider_name(
        &self,
        provider_key: &str,
        model_suffix: Option<&str>,
    ) -> Option<ModelConfig> {
        let model_name = model_suffix?;
        get_driver(provider_key)?;
        let entry = catalog::lookup(provider_key, model_name)?;
        Some(self.catalog_entry_to_config(&entry))
    }

    /// Step 3: catalog lookup by bare model name (no `/`, not a provider id).
    fn try_catalog_by_bare_model_name(
        &self,
        provider_key: &str,
        model_suffix: Option<&str>,
    ) -> Option<ModelConfig> {
        if model_suffix.is_some() || get_driver(provider_key).is_some() {
            return None;
        }
        let entry = catalog::lookup_by_model_name(self.override_str)?;
        Some(self.catalog_entry_to_config(&entry))
    }

    /// Step 4: fall back to [`resolve_model_cfg`] with `config.model` as base.
    fn fallback(&self) -> ModelConfig {
        resolve_model_cfg(&self.config.model, self.override_str)
    }

    /// Convert a catalog entry to a [`ModelConfig`], inheriting credentials
    /// from `config.model` when the provider matches.
    fn catalog_entry_to_config(&self, entry: &catalog::ModelCatalogEntry) -> ModelConfig {
        let mut cfg = ModelConfig {
            provider: entry.provider.clone(),
            name: entry.id.clone(),
            ..ModelConfig::default()
        };
        if cfg.provider == self.config.model.provider {
            cfg.api_key = self.config.model.api_key.clone();
            cfg.api_key_env = self.config.model.api_key_env.clone();
        }
        cfg
    }
}

// ── Model-config resolution ───────────────────────────────────────────────────

/// Build a [`ModelConfig`] by applying `override_str` on top of `base`.
///
/// The override string may be:
/// - `"provider/model"` → sets both provider and name (e.g. `"anthropic/claude-opus-4-5"`)
/// - bare registered provider id (e.g. `"groq"`, `"ollama"`) → changes provider, keeps model name
/// - bare model name (no `/`, not a known provider id) → changes model name, keeps provider
///
/// When the provider changes, every field that describes the *old*
/// provider/model's capacity or wire quirks is cleared - see the comment
/// below for the full list and why each one is provider-specific.
pub fn resolve_model_cfg(base: &ModelConfig, override_str: &str) -> ModelConfig {
    let mut cfg = base.clone();
    let provider_changed;
    if let Some((provider, model)) = override_str.split_once('/') {
        provider_changed = provider != base.provider;
        cfg.provider = provider.to_string();
        cfg.name = model.to_string();
    } else if get_driver(override_str).is_some() {
        // Bare provider id - change provider, keep the current model name.
        provider_changed = override_str != base.provider;
        cfg.provider = override_str.to_string();
    } else {
        cfg.name = override_str.to_string();
        provider_changed = false;
    }
    // When the provider changes, everything inherited from `base` that
    // describes the *old* provider/model belongs to it, not the new one -
    // clear all of it so the new provider starts from a clean slate (its
    // registry defaults / live probe / catalog entry), rather than silently
    // wearing the old provider's capacity limits and wire quirks:
    //
    // - `api_key`/`api_key_env`/`base_url`: resolve_api_key() falls through
    //   to the new provider's registry default env var and from_config()
    //   uses the new provider's canonical endpoint instead of the old one.
    //   Example: config.model has base_url="http://koala:8000/v1" (local
    //   GGUF) and the user runs `--model openai/gpt-5.5`; without clearing
    //   base_url the openai provider would hit the local server instead of
    //   api.openai.com.
    // - `max_tokens`/`max_output_tokens`/`max_input_tokens`: these are a
    //   specific model's measured/configured capacity (e.g. a tiny local
    //   model's 1024-token window). Carrying them into an unrelated provider
    //   makes `effective_input_budget` reject requests using a context
    //   window and output reservation that belong to a different model
    //   entirely - confirmed in practice: `--model brain/Qwen/Qwen3-0.6B`
    //   inherited a `sven`-provider config's `max_tokens: 1024,
    //   max_output_tokens: 1024` verbatim, rejecting a 1-token prompt with
    //   "budget of 0 tokens" for a model that was never actually configured
    //   that way.
    // - `driver_options`: provider-specific extra request-body fields (e.g.
    //   `chat_template_kwargs`) that a different provider's server may not
    //   understand or may interpret differently.
    // - `azure_resource`/`azure_deployment`/`azure_api_version`/`aws_region`:
    //   meaningless (or actively wrong) outside their own provider.
    if provider_changed {
        cfg.api_key = None;
        cfg.api_key_env = None;
        cfg.base_url = None;
        cfg.max_tokens = None;
        cfg.max_output_tokens = None;
        cfg.max_input_tokens = None;
        cfg.driver_options = serde_json::Value::Null;
        cfg.azure_resource = None;
        cfg.azure_deployment = None;
        cfg.azure_api_version = None;
        cfg.aws_region = None;
    }
    cfg
}

/// Resolve a [`ModelConfig`] using `override_str`, checking
/// `config.providers` for named custom providers first.
///
/// If the prefix of `override_str` (the part before an optional `/`) matches
/// a key in `config.providers`, that named config is used as the base and
/// only the model name portion is optionally overridden.
///
/// Otherwise the call falls back to [`resolve_model_cfg`] with
/// `config.model` as the base, supporting the same `"provider/name"` /
/// bare-provider / bare-name syntax.
///
/// # Example
/// ```yaml
/// providers:
///   my_ollama:
///     provider: openai   # openai-compatible endpoint
///     base_url: http://localhost:11434/v1
///     name: llama3.2
/// ```
/// `--model my_ollama` uses the whole named config;
/// `--model my_ollama/codellama` overrides just the model name.
/// Thin wrapper around [`ModelResolver`] for backwards-compatible call sites.
///
/// Prefer `ModelResolver::new(config, override_str).resolve()` for new code.
pub fn resolve_model_from_config(config: &sven_config::Config, override_str: &str) -> ModelConfig {
    ModelResolver::new(config, override_str).resolve()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_config::ModelConfig;

    // ── resolve_model_cfg ─────────────────────────────────────────────────────

    fn openai_base() -> ModelConfig {
        ModelConfig {
            provider: "openai".into(),
            name: "gpt-4o".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            ..ModelConfig::default()
        }
    }

    #[test]
    fn resolve_slash_separated_sets_provider_and_name() {
        let cfg = resolve_model_cfg(&openai_base(), "anthropic/claude-opus-4-5");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.name, "claude-opus-4-5");
    }

    #[test]
    fn resolve_slash_separated_clears_api_key_on_provider_change() {
        let cfg = resolve_model_cfg(&openai_base(), "anthropic/claude-opus-4-5");
        assert!(
            cfg.api_key_env.is_none(),
            "key env must be cleared when provider changes"
        );
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn resolve_bare_model_name_keeps_provider() {
        let cfg = resolve_model_cfg(&openai_base(), "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o-mini");
        assert_eq!(
            cfg.api_key_env.as_deref(),
            Some("OPENAI_API_KEY"),
            "key env must be preserved when provider does not change"
        );
    }

    #[test]
    fn resolve_bare_provider_id_changes_provider_and_clears_key() {
        let cfg = resolve_model_cfg(&openai_base(), "anthropic");
        assert_eq!(cfg.provider, "anthropic");
        assert!(cfg.api_key_env.is_none());
    }

    #[test]
    fn resolve_same_provider_bare_id_keeps_key() {
        let cfg = resolve_model_cfg(&openai_base(), "openai");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(
            cfg.api_key_env.as_deref(),
            Some("OPENAI_API_KEY"),
            "key env must not be cleared when provider is unchanged"
        );
    }

    /// Regression test: `--model brain/Qwen/Qwen3-0.6B` against a config
    /// whose `model:` section actually described a different local server
    /// (provider "sven", 1024-token capacity) inherited that server's
    /// max_tokens/max_output_tokens/driver_options verbatim, making the
    /// client-side budget gate reject a 1-token prompt with "budget of 0
    /// tokens" for a model that was never configured that way at all.
    fn local_server_base() -> ModelConfig {
        ModelConfig {
            provider: "sven".into(),
            name: "Qwen/Qwen3-0.6B".into(),
            base_url: Some("http://127.0.0.1:8788/v1".into()),
            max_tokens: Some(1024),
            max_output_tokens: Some(1024),
            max_input_tokens: Some(512),
            driver_options: serde_json::json!({"chat_template_kwargs": {"enable_thinking": false}}),
            azure_resource: Some("leftover".into()),
            aws_region: Some("leftover".into()),
            ..ModelConfig::default()
        }
    }

    #[test]
    fn resolve_slash_separated_clears_capacity_and_driver_options_on_provider_change() {
        let cfg = resolve_model_cfg(&local_server_base(), "brain/Qwen/Qwen3-0.6B");
        assert_eq!(cfg.provider, "brain");
        assert_eq!(
            cfg.max_tokens, None,
            "a different provider's context window must not carry over"
        );
        assert_eq!(cfg.max_output_tokens, None);
        assert_eq!(cfg.max_input_tokens, None);
        assert!(
            cfg.driver_options.is_null(),
            "a different provider's extra request fields must not carry over"
        );
        assert_eq!(cfg.azure_resource, None);
        assert_eq!(cfg.aws_region, None);
        // base_url clearing was already covered above; capacity/driver_options
        // is the new part of this regression test.
        assert_eq!(cfg.base_url, None);
    }

    #[test]
    fn resolve_same_provider_keeps_capacity_and_driver_options() {
        let cfg = resolve_model_cfg(&local_server_base(), "sven/some-other-model");
        assert_eq!(cfg.provider, "sven");
        assert_eq!(
            cfg.max_tokens,
            Some(1024),
            "capacity settings for the SAME provider must be preserved"
        );
        assert_eq!(cfg.max_output_tokens, Some(1024));
        assert!(!cfg.driver_options.is_null());
    }

    // ── resolve_model_from_config ─────────────────────────────────────────────

    fn config_with_named_provider() -> sven_config::Config {
        use std::collections::HashMap;
        let mut providers = HashMap::new();
        let mut entry = sven_config::ProviderEntry {
            name: "openai".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            api_key: Some("ollama".into()),
            ..sven_config::ProviderEntry::default()
        };
        entry
            .models
            .insert("llama3.2".into(), sven_config::ModelParams::default());
        providers.insert("my_ollama".into(), entry);
        sven_config::Config {
            model: ModelConfig {
                provider: "openai".into(),
                name: "llama3.2".into(),
                ..ModelConfig::default()
            },
            providers,
            ..sven_config::Config::default()
        }
    }

    #[test]
    fn resolve_from_config_named_provider_used_as_base() {
        let config = config_with_named_provider();
        let cfg = resolve_model_from_config(&config, "my_ollama");
        assert_eq!(cfg.provider, "openai");
        // No model suffix → uses config.model.name as fallback
        assert_eq!(cfg.name, "llama3.2");
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434/v1"));
    }

    #[test]
    fn resolve_from_config_named_provider_with_model_override() {
        let config = config_with_named_provider();
        let cfg = resolve_model_from_config(&config, "my_ollama/codellama");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "codellama");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("http://localhost:11434/v1"),
            "base_url from named provider must be kept"
        );
    }

    #[test]
    fn resolve_from_config_falls_back_to_standard_resolution() {
        let config = config_with_named_provider();
        // "anthropic/claude-opus-4-5" is not a named provider
        let cfg = resolve_model_from_config(&config, "anthropic/claude-opus-4-5");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.name, "claude-opus-4-5");
    }

    #[test]
    fn resolve_from_config_bare_model_name_uses_config_model_as_base() {
        let config = config_with_named_provider(); // default model = openai/gpt-4o
        let cfg = resolve_model_from_config(&config, "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o-mini");
    }

    /// Regression test: when the base config has a custom `base_url` (e.g. a
    /// local LLM endpoint) and the user overrides with a bare catalog model
    /// name (e.g. `gpt-4o`), the custom base_url must NOT be inherited.
    /// The resolved config should use the catalog's provider defaults.
    #[test]
    fn catalog_model_override_does_not_inherit_custom_base_url() {
        use std::collections::HashMap;
        let config = sven_config::Config {
            model: ModelConfig {
                provider: "openai".into(),
                name: "Qweb3-14B-Q8_0.gguf".into(),
                base_url: Some("https://my-local-llm.example.com/v1".into()),
                ..ModelConfig::default()
            },
            providers: HashMap::new(),
            ..sven_config::Config::default()
        };

        let cfg = resolve_model_from_config(&config, "gpt-4o");
        assert_eq!(
            cfg.provider, "openai",
            "provider must be openai (from catalog)"
        );
        assert_eq!(cfg.name, "gpt-4o", "model name must be gpt-4o");
        assert!(
            cfg.base_url.is_none(),
            "custom base_url must NOT be inherited when switching to a catalog model: {:?}",
            cfg.base_url
        );
    }

    /// Regression: fallback path (model NOT in catalog) must also clear base_url.
    ///
    /// Example: config.model has `provider: sven, base_url: http://koala:8000/v1`
    /// and the user runs `--model openai/gpt-5.5`.  gpt-5.5 is not in the
    /// catalog so resolution falls to `resolve_model_cfg` (step 4).  The
    /// custom base_url must not bleed over to the openai provider.
    #[test]
    fn fallback_path_does_not_inherit_custom_base_url_on_provider_change() {
        use std::collections::HashMap;
        let config = sven_config::Config {
            model: ModelConfig {
                provider: "sven".into(),
                name: "Qwen3.5-35B-A3B-Q4_0.gguf".into(),
                base_url: Some("http://koala:8000/v1".into()),
                ..ModelConfig::default()
            },
            providers: HashMap::new(),
            ..sven_config::Config::default()
        };

        // "gpt-5.5" is not in the catalog, so resolution falls to
        // resolve_model_cfg which previously leaked base_url.
        let cfg = resolve_model_from_config(&config, "openai/gpt-5.5");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-5.5");
        assert!(
            cfg.base_url.is_none(),
            "koala base_url must NOT bleed into openai provider via fallback path: {:?}",
            cfg.base_url
        );
    }

    /// Regression: selecting "openai/gpt-4o" (slash form) while config.model
    /// has a local endpoint must NOT inherit the custom base_url.
    #[test]
    fn catalog_model_slash_form_does_not_inherit_custom_base_url() {
        use std::collections::HashMap;
        let config = sven_config::Config {
            model: ModelConfig {
                provider: "openai".into(),
                name: "llama3.2".into(),
                base_url: Some("http://localhost:11434/v1".into()),
                ..ModelConfig::default()
            },
            providers: HashMap::new(),
            ..sven_config::Config::default()
        };

        // The completion list shows "openai/gpt-4o"; selecting it must produce
        // a clean config pointing at the real OpenAI endpoint.
        let cfg = resolve_model_from_config(&config, "openai/gpt-4o");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o");
        assert!(
            cfg.base_url.is_none(),
            "local Ollama base_url must NOT be inherited when switching to a catalog model \
             via 'provider/model' form: {:?}",
            cfg.base_url
        );
    }

    /// When the user overrides with a catalog model from a *different* provider
    /// (e.g. `claude-opus-4-6` while config has openai), the provider changes
    /// and credentials are not inherited.
    #[test]
    fn catalog_model_different_provider_clears_credentials() {
        use std::collections::HashMap;
        let config = sven_config::Config {
            model: ModelConfig {
                provider: "openai".into(),
                name: "gpt-4o".into(),
                api_key: Some("sk-openai-secret".into()),
                ..ModelConfig::default()
            },
            providers: HashMap::new(),
            ..sven_config::Config::default()
        };

        let cfg = resolve_model_from_config(&config, "claude-opus-4-6");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.name, "claude-opus-4-6");
        assert!(
            cfg.api_key.is_none(),
            "OpenAI api_key must not leak to anthropic config"
        );
    }

    // ── ModelResolver per-step unit tests ─────────────────────────────────────

    fn make_config(provider: &str, model: &str) -> sven_config::Config {
        use std::collections::HashMap;
        sven_config::Config {
            model: ModelConfig {
                provider: provider.into(),
                name: model.into(),
                ..ModelConfig::default()
            },
            providers: HashMap::new(),
            ..sven_config::Config::default()
        }
    }

    fn make_config_with_named(
        base_provider: &str,
        base_model: &str,
        alias: &str,
        entry: sven_config::ProviderEntry,
    ) -> sven_config::Config {
        let mut config = make_config(base_provider, base_model);
        config.providers.insert(alias.into(), entry);
        config
    }

    // ── Step 1: named provider ─────────────────────────────────────────────────

    /// Step 1: a named provider alias resolves to its stored config.
    #[test]
    fn step1_named_provider_used_as_base() {
        let entry = sven_config::ProviderEntry {
            name: "openai".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            ..sven_config::ProviderEntry::default()
        };
        // Base config model name is "gpt-4o"; no suffix → fallback to that.
        let config = make_config_with_named("openai", "gpt-4o", "my_ollama", entry);
        let cfg = ModelResolver::new(&config, "my_ollama").resolve();
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o"); // falls back to config.model.name
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434/v1"));
    }

    /// Step 1: `alias/model` form overrides the model name inside the named config.
    #[test]
    fn step1_named_provider_with_model_suffix() {
        let entry = sven_config::ProviderEntry {
            name: "openai".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            ..sven_config::ProviderEntry::default()
        };
        let config = make_config_with_named("openai", "gpt-4o", "my_ollama", entry);
        let cfg = ModelResolver::new(&config, "my_ollama/codellama").resolve();
        assert_eq!(cfg.name, "codellama");
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("http://localhost:11434/v1"),
            "base_url from named provider preserved with model suffix"
        );
    }

    /// Step 1 skip: an unknown prefix falls through to later steps.
    #[test]
    fn step1_unknown_prefix_falls_through() {
        let config = make_config("openai", "gpt-4o");
        // "anthropic" is not in config.providers, so step 1 is skipped.
        // The call should still succeed via catalog or fallback.
        let cfg = ModelResolver::new(&config, "anthropic/claude-opus-4-5").resolve();
        assert_eq!(cfg.provider, "anthropic");
    }

    // ── Step 2: catalog lookup by provider/name ────────────────────────────────

    /// Step 2: `provider/name` form resolves via catalog when provider is a known driver.
    #[test]
    fn step2_slash_form_resolves_via_catalog() {
        let config = make_config("anthropic", "claude-opus-4-5");
        // openai/gpt-4o should be in the static catalog.
        let cfg = ModelResolver::new(&config, "openai/gpt-4o").resolve();
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o");
        assert!(
            cfg.base_url.is_none(),
            "catalog model must not inherit custom base_url"
        );
    }

    /// Step 2: unknown provider in `provider/name` form bypasses catalog (step 2) and falls through.
    #[test]
    fn step2_unknown_provider_slash_form_falls_through_to_fallback() {
        let config = make_config("openai", "gpt-4o");
        // "mylocal/some-model" - "mylocal" is not a known driver.
        let cfg = ModelResolver::new(&config, "mylocal/some-model").resolve();
        // Falls through to step 4 (resolve_model_cfg) which splits at "/" directly.
        assert_eq!(cfg.provider, "mylocal");
        assert_eq!(cfg.name, "some-model");
    }

    /// Step 2: credentials are inherited when the catalog model uses the same provider as config.
    #[test]
    fn step2_inherits_credentials_when_same_provider() {
        let mut config = make_config("openai", "gpt-4o");
        config.model.api_key = Some("sk-mykey".into());
        // openai/gpt-4o-mini - same provider, should inherit api_key.
        let cfg = ModelResolver::new(&config, "openai/gpt-4o-mini").resolve();
        assert_eq!(cfg.provider, "openai");
        assert_eq!(
            cfg.api_key.as_deref(),
            Some("sk-mykey"),
            "api_key must be inherited for same-provider catalog model"
        );
    }

    // ── Step 3: catalog lookup by bare model name ──────────────────────────────

    /// Step 3: a bare model name (not a provider id) resolves via catalog.
    #[test]
    fn step3_bare_model_name_resolves_via_catalog() {
        let config = make_config("anthropic", "claude-opus-4-5");
        // "gpt-4o" is a bare model name that exists in the catalog.
        let cfg = ModelResolver::new(&config, "gpt-4o").resolve();
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o");
    }

    /// Step 3 skip: a bare known provider id (e.g. "groq") skips step 3 and falls to step 4.
    #[test]
    fn step3_bare_provider_id_skips_catalog_model_lookup() {
        let config = make_config("openai", "gpt-4o");
        // "groq" is a provider id, not a model name → step 3 is skipped.
        let cfg = ModelResolver::new(&config, "groq").resolve();
        // Fallback (step 4): provider becomes groq, model name unchanged.
        assert_eq!(cfg.provider, "groq");
    }

    // ── Step 4: fallback ───────────────────────────────────────────────────────

    /// Step 4: a bare provider id with no catalog entry changes the provider.
    #[test]
    fn step4_fallback_bare_provider_changes_provider() {
        let config = make_config("openai", "gpt-4o");
        let cfg = ModelResolver::new(&config, "groq").resolve();
        assert_eq!(cfg.provider, "groq");
    }

    /// Step 4: `provider/model` for an unknown provider sets both fields directly.
    #[test]
    fn step4_fallback_unknown_provider_slash_name_sets_both() {
        let config = make_config("openai", "gpt-4o");
        let cfg = ModelResolver::new(&config, "myprovider/mycustom-model").resolve();
        assert_eq!(cfg.provider, "myprovider");
        assert_eq!(cfg.name, "mycustom-model");
    }
}
