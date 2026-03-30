// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod anthropic;
mod aws;
pub mod catalog;
mod cohere;
mod google;
mod mock;
mod openai;
pub(crate) mod openai_compat;
mod provider;
pub mod registry;
pub mod sanitize;
mod types;
mod yaml_mock;

pub use anthropic::AnthropicProvider;
pub use catalog::{InputModality, ModelCatalogEntry};
pub use mock::{MockProvider, ScriptedMockProvider};
pub use openai::OpenAiProvider;
pub use provider::ModelProvider;
pub use registry::{get_driver, list_drivers, DriverMeta};
pub use types::*;
pub use yaml_mock::YamlMockProvider;

use anyhow::bail;
use async_trait::async_trait;
use futures::Stream;
use openai_compat::{AuthStyle, OpenAICompatProvider};
use std::pin::Pin;
use std::time::Duration;
use sven_config::ModelConfig;

// ── Shared HTTP client factory ────────────────────────────────────────────────

/// Build a [`reqwest::Client`] that is safe for long-lived SSE streaming.
///
/// All model providers share these settings:
///
/// * **TCP keepalive (30 s)** — causes the OS to probe a silent connection
///   after 30 seconds of inactivity.  This detects half-open TCP connections
///   (the remote end disappeared without a FIN/RST) and surfaces them as I/O
///   errors so the streaming loop can recover rather than hanging indefinitely.
/// * **Connect timeout (30 s)** — prevents indefinite blocking if the API
///   endpoint is unreachable or DNS resolution stalls.
///
/// No total request timeout is set because SSE streaming responses legitimately
/// run for minutes (or hours for long agentic tasks).  The per-chunk idle
/// timeout is enforced separately in the agent's streaming loop.
pub(crate) fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client")
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Perform early-exit API key validation before attempting any network call.
///
/// When the user has configured neither an explicit key nor a key-env override,
/// and the provider's registry entry lists a default env var, we check that env
/// var immediately.  If absent, we bail with an actionable message instead of
/// letting the first HTTP request surface an opaque 401.
fn check_api_key_requirement(cfg: &ModelConfig) -> anyhow::Result<()> {
    if cfg.api_key.is_some() || cfg.api_key_env.is_some() {
        return Ok(());
    }
    if let Some(meta) = registry::get_driver(&cfg.provider) {
        if let Some(env_var) = meta.default_api_key_env {
            if std::env::var(env_var).is_err() {
                bail!(
                    "No API key found for provider '{}' (model '{}').\n\
                     Please set the {env_var} environment variable:\n\
                     \n\
                     export {env_var}=<your-api-key>\n\
                     \n\
                     Alternatively, add it to your config file (~/.config/sven/config.yaml):\n\
                     \n\
                     model:\n\
                       provider: {}\n\
                       name: {}\n\
                       api_key: <your-api-key>",
                    cfg.provider,
                    cfg.name,
                    cfg.provider,
                    cfg.name,
                );
            }
        }
    }
    Ok(())
}

/// Rewrite the `auto_router_allowed_models` convenience key in OpenRouter's
/// `driver_options` into the nested `plugins` structure the API expects:
///
/// ```yaml
/// driver_options:
///   auto_router_allowed_models: ["anthropic/*", "openai/gpt-5.1"]
/// ```
/// becomes:
/// ```json
/// { "plugins": [{ "id": "auto-router", "allowed_models": [...] }] }
/// ```
///
/// A raw `plugins` key is passed through unchanged.
fn transform_openrouter_options(cfg: &ModelConfig) -> serde_json::Value {
    let mut opts = cfg.driver_options.clone();
    if let Some(allowed) = opts.get("auto_router_allowed_models").cloned() {
        if let Some(map) = opts.as_object_mut() {
            map.remove("auto_router_allowed_models");
            map.entry("plugins").or_insert_with(
                || serde_json::json!([{ "id": "auto-router", "allowed_models": allowed }]),
            );
        }
    }
    opts
}

// ── ConfigBoundedProvider ─────────────────────────────────────────────────────

/// Wraps any [`ModelProvider`] and overrides its catalog-reported context
/// limits with values derived from the user's config.
///
/// This ensures that `catalog_context_window()` and
/// `catalog_max_output_tokens()` reflect what the user explicitly configured
/// rather than (potentially absent) static catalog metadata.  All other
/// trait methods are forwarded directly to the inner provider.
struct ConfigBoundedProvider {
    inner: Box<dyn ModelProvider>,
    /// Total context window from `cfg.max_tokens` (when `max_output_tokens` is
    /// also set, `max_tokens` is interpreted as the pure total context limit).
    context_window: Option<u32>,
    /// Resolved output token cap: `cfg.max_output_tokens` if set, else
    /// `cfg.max_tokens` for backward compatibility.
    max_output_tokens: Option<u32>,
}

#[async_trait]
impl ModelProvider for ConfigBoundedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn complete(
        &self,
        req: crate::CompletionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<crate::ResponseEvent>> + Send>>>
    {
        self.inner.complete(req).await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<crate::ModelCatalogEntry>> {
        self.inner.list_models().await
    }

    fn catalog_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
            .or_else(|| self.inner.catalog_max_output_tokens())
    }

    fn catalog_context_window(&self) -> Option<u32> {
        self.context_window
            .or_else(|| self.inner.catalog_context_window())
    }

    async fn probe_context_window(&self) -> Option<u32> {
        self.inner.probe_context_window().await
    }

    fn input_modalities(&self) -> Vec<crate::catalog::InputModality> {
        self.inner.input_modalities()
    }

    fn config_context_window(&self) -> Option<u32> {
        self.context_window
    }

    fn config_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }
}

// ── from_config ───────────────────────────────────────────────────────────────

/// Construct a boxed [`ModelProvider`] from configuration.
///
/// Selects the driver implementation based on `cfg.provider`.  Run
/// `sven list-providers` to see all recognised provider ids.
///
/// The resolved output token limit is determined in priority order:
/// 1. `cfg.max_output_tokens` — explicit per-request output cap
/// 2. `cfg.max_tokens` — backward-compatible total/output cap
/// 3. Static catalog `max_output_tokens` for the model
/// 4. Hardcoded fallback of 4096
///
/// When `cfg.max_output_tokens` is set, `cfg.max_tokens` is exposed as the
/// total context window (used by compaction decisions).  When only
/// `cfg.max_tokens` is set, it serves as both the output cap and context
/// window (original behaviour, fully backward-compatible).
pub fn from_config(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    check_api_key_requirement(cfg)?;

    // key() returns a fresh Option<String> on each call so that each match arm
    // can take ownership without cross-arm borrow issues.
    let key = || resolve_api_key(cfg);

    // Resolve the output token limit sent to the provider API:
    //   1. cfg.max_output_tokens  — explicit per-request output cap
    //   2. cfg.max_tokens         — backward compat: total used as output cap
    //   3. catalog max_output_tokens for the model
    // The final unwrap_or(4096) lives inside OpenAICompatProvider::new.
    let resolved_max_tokens = cfg
        .max_output_tokens
        .or(cfg.max_tokens)
        .or_else(|| catalog::lookup(&cfg.provider, &cfg.name).map(|e| e.max_output_tokens));

    // Context window exposed via catalog_context_window():
    // - When max_output_tokens is set, max_tokens is the *total* context.
    // - When only max_tokens is set, it doubles as both output cap and context.
    // Either way, exposing max_tokens here is correct.
    let config_ctx = cfg.max_tokens;

    // Helper that reads `base_url` from config or falls back to a static default.
    let base_url =
        |default: &str| -> String { cfg.base_url.clone().unwrap_or_else(|| default.into()) };

    let inner: Box<dyn ModelProvider> = match cfg.provider.as_str() {
        // ── Native drivers ────────────────────────────────────────────────────
        "openai" => Box::new(OpenAiProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
            cfg.driver_options.clone(),
        )),
        "anthropic" => Box::new(AnthropicProvider::with_cache(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
            cfg.cache_system_prompt,
            cfg.extended_cache_time,
            cfg.cache_tools,
            cfg.cache_conversation,
            cfg.cache_images,
            cfg.cache_tool_results,
        )),
        "google" => Box::new(google::GoogleProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
        )),
        "aws" => Box::new(aws::BedrockProvider::new(
            cfg.name.clone(),
            cfg.aws_region.clone(),
            resolved_max_tokens,
            cfg.temperature,
        )),
        "cohere" => Box::new(cohere::CohereProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
        )),

        // ── Azure OpenAI (OpenAI-compat with special URL + api-key header) ────
        "azure" => {
            let chat_url = if let Some(b) = &cfg.base_url {
                let api_ver = cfg.azure_api_version.as_deref().unwrap_or("2024-02-01");
                format!(
                    "{}/chat/completions?api-version={}",
                    b.trim_end_matches('/'),
                    api_ver
                )
            } else {
                let resource = cfg.azure_resource.as_deref().unwrap_or("myresource");
                let deployment = cfg.azure_deployment.as_deref().unwrap_or(&cfg.name);
                let api_ver = cfg.azure_api_version.as_deref().unwrap_or("2024-02-01");
                format!(
                    "https://{resource}.openai.azure.com/openai/deployments/{deployment}/chat/completions?api-version={api_ver}"
                )
            };
            Box::new(OpenAICompatProvider::with_full_chat_url(
                "azure",
                cfg.name.clone(),
                key(),
                chat_url,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                openai_compat::AuthStyle::ApiKeyHeader,
                cfg.driver_options.clone(),
            ))
        }

        // ── OpenAI-compatible gateways (special-cased for custom behaviour) ──
        "openrouter" => {
            let or_base = base_url("https://openrouter.ai/api/v1");
            // Load any fresh disk cache before using catalog metadata.
            catalog::load_disk_cache("openrouter");
            // Spawn a background task to refresh the cache when stale.
            maybe_spawn_openrouter_cache_refresh(key(), or_base.clone());
            Box::new(OpenAICompatProvider::new(
                "openrouter",
                cfg.name.clone(),
                key(),
                &or_base,
                resolved_max_tokens,
                cfg.temperature,
                vec![
                    (
                        "HTTP-Referer".into(),
                        "https://github.com/svenai/sven".into(),
                    ),
                    ("X-Title".into(), "sven".into()),
                ],
                AuthStyle::Bearer,
                transform_openrouter_options(cfg),
            ))
        }
        "portkey" => Box::new(OpenAICompatProvider::new(
            "portkey",
            cfg.name.clone(),
            key(),
            &base_url("https://api.portkey.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            portkey_extra_headers(cfg),
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        )),
        "litellm" => {
            let b = cfg
                .base_url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("litellm provider requires base_url in config"))?;
            Box::new(OpenAICompatProvider::new(
                "litellm",
                cfg.name.clone(),
                key(),
                b,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                AuthStyle::Bearer,
                cfg.driver_options.clone(),
            ))
        }
        "cloudflare" => {
            let b = cfg.base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "cloudflare provider requires base_url in config (account-specific URL)"
                )
            })?;
            Box::new(OpenAICompatProvider::new(
                "cloudflare",
                cfg.name.clone(),
                key(),
                b,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                AuthStyle::Bearer,
                cfg.driver_options.clone(),
            ))
        }
        // vLLM accepts an optional bearer token; auth style depends on whether
        // a key is actually configured.
        "vllm" => {
            let k = key();
            let auth = if k.is_some() {
                AuthStyle::Bearer
            } else {
                AuthStyle::None
            };
            Box::new(OpenAICompatProvider::new(
                "vllm",
                cfg.name.clone(),
                k,
                &base_url("http://localhost:8000/v1"),
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                auth,
                cfg.driver_options.clone(),
            ))
        }

        // ── Testing / Mock ────────────────────────────────────────────────────
        "mock" => {
            let responses_path = std::env::var("SVEN_MOCK_RESPONSES")
                .ok()
                .or_else(|| cfg.mock_responses_file.clone());
            if let Some(path) = responses_path {
                Box::new(YamlMockProvider::from_file(&path)?) as Box<dyn ModelProvider>
            } else {
                Box::new(MockProvider) as Box<dyn ModelProvider>
            }
        }

        // ── Registry-driven OpenAI-compat catch-all ───────────────────────────
        //
        // All remaining registered providers are OpenAI-compatible and differ
        // only in their default base URL and whether they require a bearer
        // token.  Both values are already stored in the driver registry, so we
        // can construct the provider generically rather than repeating the same
        // eight-line block for every provider.
        other => {
            let meta = registry::get_driver(other).ok_or_else(|| {
                let known: Vec<&str> = registry::known_driver_ids().collect();
                anyhow::anyhow!(
                    "unknown model provider: {other:?}\n\
                     Run `sven list-providers` for a full list, or check your config.\n\
                     Known providers: {}",
                    known.join(", ")
                )
            })?;
            let default_url = meta
                .default_base_url
                .ok_or_else(|| anyhow::anyhow!("{other} provider requires base_url in config"))?;
            let auth = if meta.requires_api_key {
                AuthStyle::Bearer
            } else {
                AuthStyle::None
            };
            Box::new(OpenAICompatProvider::new(
                meta.id,
                cfg.name.clone(),
                key(),
                &base_url(default_url),
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                auth,
                cfg.driver_options.clone(),
            ))
        }
    };

    // Wrap the inner provider with config-specified limits so that
    // catalog_context_window() and catalog_max_output_tokens() reflect the
    // user's explicit configuration rather than the static catalog alone.
    // This ensures compaction thresholds and session budget calculations use
    // the correct values even for models not present in the bundled catalog.
    Ok(Box::new(ConfigBoundedProvider {
        inner,
        context_window: config_ctx,
        max_output_tokens: resolved_max_tokens,
    }))
}

fn resolve_api_key(cfg: &ModelConfig) -> Option<String> {
    if let Some(k) = &cfg.api_key {
        return Some(k.clone());
    }
    if let Some(env) = &cfg.api_key_env {
        return std::env::var(env).ok();
    }
    // Auto-resolve from registry default env var if neither is set.
    if let Some(meta) = registry::get_driver(&cfg.provider) {
        if let Some(env_var) = meta.default_api_key_env {
            return std::env::var(env_var).ok();
        }
    }
    None
}

/// Spawn a background tokio task to refresh the OpenRouter model catalog cache.
///
/// The task fetches `GET <base_url>/models`, parses the rich OpenRouter
/// metadata (context window, output token cap, input modalities), then calls
/// [`catalog::cache_update`] to update both the in-memory live cache and the
/// on-disk file.
///
/// Only spawned when:
/// 1. A tokio runtime is already running (safe to call `tokio::spawn`).
/// 2. [`catalog::is_cache_stale`] reports that the on-disk cache is absent or
///    older than 24 hours.
/// 3. An API key is available (needed to authenticate the request).
///
/// Errors in the background task are silently discarded — cache refresh is a
/// best-effort optimisation, not a critical path.
fn maybe_spawn_openrouter_cache_refresh(api_key: Option<String>, base_url: String) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return; // No runtime active — skip (e.g. unit tests, sync callers).
    };
    let Some(key) = api_key else {
        return; // No key — cannot authenticate the request.
    };
    if !catalog::is_cache_stale("openrouter") {
        return; // Fresh disk cache — no need to refresh.
    }
    handle.spawn(async move {
        let models_url = format!("{}/models", base_url.trim_end_matches('/'));
        let client = build_http_client();
        let mut req = client
            .get(&models_url)
            .bearer_auth(&key)
            .header("HTTP-Referer", "https://github.com/svenai/sven")
            .header("X-Title", "sven")
            .timeout(std::time::Duration::from_secs(30));
        // Also set the header as `req =` to allow chaining (reqwest returns
        // the builder by value).
        req = req.header("User-Agent", "sven-model/cache-refresh");

        let Ok(resp) = req.send().await else { return };
        if !resp.status().is_success() {
            return;
        }
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return;
        };

        // Use the YAML-only entries for the meta-model fallback list so we
        // don't create a circular dependency with the live cache.
        let yaml_or_entries: Vec<catalog::ModelCatalogEntry> = catalog::yaml_catalog()
            .iter()
            .filter(|e| e.provider == "openrouter")
            .cloned()
            .collect();

        let entries = openai_compat::parse_models_response(&body, "openrouter", &yaml_or_entries);
        if !entries.is_empty() {
            tracing::debug!(
                "openrouter cache refresh: {} models fetched and cached",
                entries.len()
            );
            catalog::cache_update("openrouter", entries);
        }
    });
}

fn portkey_extra_headers(cfg: &ModelConfig) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(vk) = cfg
        .driver_options
        .get("portkey_virtual_key")
        .and_then(|v| v.as_str())
    {
        headers.push(("x-portkey-virtual-key".into(), vk.to_string()));
    }
    headers
}

// ── ModelResolver ─────────────────────────────────────────────────────────────

/// Resolves a user-supplied model string to a [`ModelConfig`].
///
/// Resolution happens in four ordered steps; the first one that succeeds wins:
///
/// 1. **Named provider** — if the prefix of `override_str` matches a key in
///    `config.providers`, use that named config (optionally overriding the
///    model name with the suffix after `/`).
/// 2. **Catalog lookup `provider/name`** — when `override_str` contains `/`
///    and the prefix is a known driver, look up `(provider, name)` in the
///    static model catalog.  A fresh `ModelConfig` is built from catalog
///    metadata; credentials are inherited only when the resolved provider
///    matches `config.model.provider`.
/// 3. **Catalog lookup by bare model name** — when `override_str` has no `/`
///    and is not a known provider id, search the catalog for that model name
///    alone (provider is inferred from the catalog entry).
/// 4. **Fallback** — call [`resolve_model_cfg`] with `config.model` as the
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
/// When the provider changes, inherited `api_key` / `api_key_env` fields are
/// cleared so the correct credential env-var for the new provider is looked up.
pub fn resolve_model_cfg(base: &ModelConfig, override_str: &str) -> ModelConfig {
    let mut cfg = base.clone();
    let provider_changed;
    if let Some((provider, model)) = override_str.split_once('/') {
        provider_changed = provider != base.provider;
        cfg.provider = provider.to_string();
        cfg.name = model.to_string();
    } else if get_driver(override_str).is_some() {
        // Bare provider id — change provider, keep the current model name.
        provider_changed = override_str != base.provider;
        cfg.provider = override_str.to_string();
    } else {
        cfg.name = override_str.to_string();
        provider_changed = false;
    }
    // When the provider changes the inherited api_key / api_key_env belong to
    // the original provider.  Clear them so resolve_api_key() falls through to
    // the new provider's registry default env var.
    if provider_changed {
        cfg.api_key = None;
        cfg.api_key_env = None;
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

    fn minimal_config(provider: &str, model: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.into(),
            name: model.into(),
            ..ModelConfig::default()
        }
    }

    #[test]
    fn from_config_openai_succeeds() {
        let cfg = minimal_config("openai", "gpt-4o");
        // Either succeeds (if OPENAI_API_KEY is set) or fails with a missing-key
        // error — the provider must always be recognised.
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    #[test]
    fn from_config_anthropic_succeeds() {
        let cfg = minimal_config("anthropic", "claude-opus-4-5");
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    #[test]
    fn from_config_google_succeeds() {
        let cfg = minimal_config("google", "gemini-2.0-flash-exp");
        // Either succeeds (if GEMINI_API_KEY is set in env) or fails with a
        // missing-key error — the provider must always be recognised.
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    #[test]
    fn from_config_mock_succeeds() {
        let cfg = minimal_config("mock", "mock-model");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_groq_succeeds() {
        let cfg = minimal_config("groq", "llama-3.3-70b-versatile");
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    #[test]
    fn from_config_ollama_requires_no_key() {
        let cfg = minimal_config("ollama", "llama3.2");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_deepseek_succeeds() {
        let cfg = minimal_config("deepseek", "deepseek-chat");
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    #[test]
    fn from_config_unknown_provider_returns_error() {
        let cfg = minimal_config("totally_unknown_provider_xyz", "some-model");
        let result = from_config(&cfg);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("unknown model provider"));
    }

    #[test]
    fn from_config_error_message_suggests_list_providers() {
        let cfg = minimal_config("badprovider", "m");
        let msg = from_config(&cfg).err().unwrap().to_string();
        assert!(msg.contains("list-providers") || msg.contains("Known providers"));
    }

    #[test]
    fn resolve_api_key_prefers_explicit_key() {
        let cfg = ModelConfig {
            api_key: Some("explicit-key".into()),
            api_key_env: Some("NONEXISTENT_ENV_VAR_XYZ".into()),
            ..ModelConfig::default()
        };
        let key = resolve_api_key(&cfg);
        assert_eq!(key.as_deref(), Some("explicit-key"));
    }

    #[test]
    fn all_registry_drivers_have_constructors() {
        // Every driver id in the registry must be handled by from_config
        // without returning "unknown provider" (API key errors are OK).
        for meta in list_drivers() {
            if meta.id == "litellm" || meta.id == "cloudflare" {
                // These require base_url — skip here.
                continue;
            }
            if meta.id == "azure" {
                // Azure requires resource name — skip.
                continue;
            }
            let cfg = minimal_config(meta.id, "test-model");
            let result = from_config(&cfg);
            match result {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        !msg.contains("unknown model provider"),
                        "driver {id} is in registry but not handled by from_config: {msg}",
                        id = meta.id
                    );
                }
            }
        }
    }

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
        // "mylocal/some-model" — "mylocal" is not a known driver.
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
        // openai/gpt-4o-mini — same provider, should inherit api_key.
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
