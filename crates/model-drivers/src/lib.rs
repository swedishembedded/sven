// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Concrete [`sven_model::ModelProvider`] driver implementations (native
//! OpenAI/Anthropic/Google/AWS Bedrock/Cohere/D-Bus clients plus the shared
//! OpenAI-compatible wire format that every other provider in
//! `sven_model::registry` speaks) and the `from_config`/`from_config_probed`
//! factory that selects and constructs one from a `sven_config::ModelConfig`,
//! plus the resolution of a user-supplied model string into that
//! configuration ([`ModelResolver`], [`resolve_model_from_config`]).
//!
//! This is where `reqwest`, `aws-sdk`-style SigV4 signing, and every
//! provider's own dependency closure live, so crates that only need the
//! `ModelProvider` trait + request/response types (`sven-model`) do not pay
//! for any of it transitively.
mod anthropic;
mod api_key;
mod aws;
mod cohere;
mod dbus;
mod google;
mod openai;
pub(crate) mod openai_compat;
mod resolve;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use resolve::{resolve_model_cfg, resolve_model_from_config, ModelResolver};

use anyhow::bail;
use api_key::{read_key_from_file, resolve_api_key};
use async_trait::async_trait;
use futures::Stream;
use openai_compat::{AuthStyle, OpenAICompatProvider};
use std::pin::Pin;
use std::time::Duration;
use sven_config::ModelConfig;
use sven_model::{catalog, registry, ModelProvider};
use sven_model_mock::{MockProvider, YamlMockProvider};

// ── Shared HTTP client factory ────────────────────────────────────────────────

/// Build a [`reqwest::Client`] that is safe for long-lived SSE streaming.
///
/// All model providers share these settings:
///
/// * **TCP keepalive (30 s)** - causes the OS to probe a silent connection
///   after 30 seconds of inactivity.  This detects half-open TCP connections
///   (the remote end disappeared without a FIN/RST) and surfaces them as I/O
///   errors so the streaming loop can recover rather than hanging indefinitely.
/// * **Connect timeout (30 s)** - prevents indefinite blocking if the API
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
/// letting the first HTTP request surface an opaque 401. A provider-specific
/// keys file (see [`key_file_for`]/[`read_key_from_file`]) also satisfies the
/// requirement — must mirror [`resolve_api_key`]'s precedence exactly, or this
/// bails on a config that would actually have resolved a key.
fn check_api_key_requirement(cfg: &ModelConfig) -> anyhow::Result<()> {
    if cfg.api_key.is_some() || cfg.api_key_env.is_some() {
        return Ok(());
    }
    if let Some(meta) = registry::get_driver(&cfg.provider) {
        if let Some(env_var) = meta.default_api_key_env {
            let has_env_key = std::env::var(env_var).is_ok();
            let has_file_key = read_key_from_file(&cfg.provider).is_some();
            if !has_env_key && !has_file_key {
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
    /// Input modalities declared in config (`cfg.input_modalities`).
    ///
    /// The bundled catalog cannot know about self-hosted multimodal models, so
    /// a config declaration is authoritative when present.  `None` means "no
    /// declaration" and the inner driver's answer is used instead.
    input_modalities: Option<Vec<sven_model::catalog::InputModality>>,
}

/// Parse the config's `input_modalities` strings into [`InputModality`] values.
///
/// Unknown entries are dropped with a warning rather than failing the whole
/// config; an all-unknown list yields `None` so the driver's own answer wins.
fn parse_input_modalities(values: &[String]) -> Option<Vec<sven_model::catalog::InputModality>> {
    use sven_model::catalog::InputModality;
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        match v.trim().to_ascii_lowercase().as_str() {
            "text" => out.push(InputModality::Text),
            "image" | "vision" => out.push(InputModality::Image),
            "audio" => out.push(InputModality::Audio),
            other => tracing::warn!(
                modality = other,
                "unknown value in `input_modalities`; expected one of text, image, audio"
            ),
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
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
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>>>
    {
        self.inner.complete(req).await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<sven_model::ModelCatalogEntry>> {
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

    fn input_modalities(&self) -> Vec<sven_model::catalog::InputModality> {
        self.input_modalities
            .clone()
            .unwrap_or_else(|| self.inner.input_modalities())
    }

    fn config_context_window(&self) -> Option<u32> {
        self.context_window
    }

    fn config_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }
}

// ── from_config ───────────────────────────────────────────────────────────────

/// Select and construct the driver implementation for `cfg.provider`, plus
/// the two config-derived limits [`from_config`]/[`from_config_probed`] wrap
/// it with. Shared by both so neither duplicates the driver-selection match
/// or double-wraps the result in [`ConfigBoundedProvider`].
///
/// The resolved output token limit (third tuple element) is determined in
/// priority order:
/// 1. `cfg.max_output_tokens` - explicit per-request output cap
/// 2. `cfg.max_tokens` - backward-compatible total/output cap
/// 3. Static catalog `max_output_tokens` for the model
/// 4. Hardcoded fallback of 4096
///
/// When `cfg.max_output_tokens` is set, `cfg.max_tokens` is exposed as the
/// total context window (used by compaction decisions).  When only
/// `cfg.max_tokens` is set, it serves as both the output cap and context
/// window (original behaviour, fully backward-compatible).
///
/// `(driver, config_context_window, resolved_max_output_tokens)`.
type BuiltProvider = (Box<dyn ModelProvider>, Option<u32>, Option<u32>);

fn build_inner(cfg: &ModelConfig) -> anyhow::Result<BuiltProvider> {
    check_api_key_requirement(cfg)?;

    // key() returns a fresh Option<String> on each call so that each match arm
    // can take ownership without cross-arm borrow issues.
    let key = || resolve_api_key(cfg);

    // Resolve the output token limit sent to the provider API:
    //   1. cfg.max_output_tokens  - explicit per-request output cap
    //   2. cfg.max_tokens         - backward compat: total used as output cap
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
        // "sven" is a user-defined alias for a local OpenAI-compatible server
        // (e.g. a vLLM / llama.cpp / Ollama instance running on a custom host).
        // It behaves identically to "vllm" but is kept as a distinct id so
        // users who wrote `provider: sven` in their config don't hit an
        // "unknown provider" error.  base_url must be set in config.
        "sven" => {
            let base = cfg.base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "provider 'sven' requires base_url in config.\n\
                     Set it to your local server, e.g.:\n\
                     \n\
                     model:\n\
                       provider: sven\n\
                       base_url: http://koala:8000/v1\n\
                       name: <your-model-name>"
                )
            })?;
            let k = key();
            let auth = if k.is_some() {
                AuthStyle::Bearer
            } else {
                AuthStyle::None
            };
            Box::new(OpenAICompatProvider::new(
                "sven",
                cfg.name.clone(),
                k,
                base,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                auth,
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

        // brain over D-Bus: not HTTP, so never the catch-all below.
        "dbus" => dbus::provider(cfg, resolved_max_tokens, cfg.temperature)?,

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

    Ok((inner, config_ctx, resolved_max_tokens))
}

/// Construct a boxed [`ModelProvider`] from configuration.
///
/// Selects the driver implementation based on `cfg.provider`.  Run
/// `sven list-providers` to see all recognised provider ids. See
/// [`build_inner`] for the output-token resolution priority order.
///
/// Wraps the driver in [`ConfigBoundedProvider`] so that
/// `catalog_context_window()` and `catalog_max_output_tokens()` reflect the
/// user's explicit configuration rather than the static catalog alone. This
/// ensures compaction thresholds and session budget calculations use the
/// correct values even for models not present in the bundled catalog.
///
/// Does no I/O beyond what driver construction itself requires (none, for
/// every current driver) — for a version that additionally probes the live
/// server for its actual context window, see [`from_config_probed`].
pub fn from_config(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    let (inner, context_window, max_output_tokens) = build_inner(cfg)?;
    Ok(Box::new(ConfigBoundedProvider {
        inner,
        context_window,
        max_output_tokens,
        input_modalities: cfg
            .input_modalities
            .as_deref()
            .and_then(parse_input_modalities),
    }))
}

/// Like [`from_config`], but additionally probes the live provider for its
/// actual context window (see [`ModelProvider::probe_context_window`]) and
/// clamps the exposed `catalog_context_window()` to `min(config, probed)`
/// when the probe succeeds — never widens it, only narrows a hand-written
/// number down to what the server can actually serve.
///
/// This is what stops a config `max_tokens` from exceeding a live server's
/// real capacity (e.g. brain's per-model capacity, discovered via
/// `/v1/models`'s `context_length` — see
/// `openai_compat::probe_context_window_via_models_list`). When the probe
/// fails (hosted providers, an unreachable/non-conforming server) or the
/// config has no `max_tokens` at all, the probed value is used as-is;
/// when neither is present the result is identical to `from_config`.
///
/// Also resolves an EMPTY `cfg.name` for `provider: "brain"` (the sentinel
/// `sven-config`'s loader writes when it auto-detects brain without knowing
/// which resident model it's serving — loader.rs is sync/network-free and
/// genuinely cannot know) by asking brain's own `/v1/models`. Only resolves
/// when the answer is unambiguous (exactly one chat-capable entry, i.e.
/// exactly one entry advertising `context_length` — see the paired brain
/// commit populating that truthfully); otherwise `cfg.name` stays empty and
/// the eventual request fails with brain's own `model_not_found`, which
/// Part 1's fail-loudly fix now surfaces clearly instead of silently.
///
/// Does one to two extra network round-trips before returning;
/// `probe_context_window` and the model-list fetch are both short-timeout
/// (5s) best-effort calls, never a hard failure.
pub async fn from_config_probed(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    let cfg = &resolve_empty_brain_model_name(cfg).await;
    let (inner, config_ctx, max_output_tokens) = build_inner(cfg)?;
    let probed = inner.probe_context_window().await;
    let context_window = match (config_ctx, probed) {
        (Some(c), Some(p)) => Some(c.min(p)),
        (Some(c), None) => Some(c),
        (None, ctx) => ctx,
    };
    Ok(Box::new(ConfigBoundedProvider {
        inner,
        context_window,
        max_output_tokens,
        input_modalities: cfg
            .input_modalities
            .as_deref()
            .and_then(parse_input_modalities),
    }))
}

/// See [`from_config_probed`]'s doc comment. Returns `cfg` unchanged unless
/// `provider == "brain"`, `name` is empty, and exactly one chat-capable
/// model (one `/v1/models` entry with `context_length` present) is found.
async fn resolve_empty_brain_model_name(cfg: &ModelConfig) -> ModelConfig {
    if cfg.provider != "brain" || !cfg.name.is_empty() {
        return cfg.clone();
    }
    let Some(meta) = registry::get_driver("brain") else {
        return cfg.clone();
    };
    let base = cfg
        .base_url
        .clone()
        .or_else(|| meta.default_base_url.map(str::to_string));
    let Some(base) = base else {
        return cfg.clone();
    };
    let key = resolve_api_key(cfg);
    let client = build_http_client();
    let mut req = client
        .get(format!("{}/models", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(5));
    if let Some(k) = &key {
        req = req.bearer_auth(k);
    }
    let Ok(resp) = req.send().await else {
        return cfg.clone();
    };
    if !resp.status().is_success() {
        return cfg.clone();
    }
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return cfg.clone();
    };
    let Some(entries) = body["data"].as_array() else {
        return cfg.clone();
    };
    let chat_capable: Vec<&str> = entries
        .iter()
        .filter(|m| m.get("context_length").is_some())
        .filter_map(|m| m["id"].as_str())
        .collect();
    match chat_capable.as_slice() {
        [only] => ModelConfig {
            name: (*only).to_string(),
            ..cfg.clone()
        },
        _ => cfg.clone(),
    }
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
/// Errors in the background task are silently discarded - cache refresh is a
/// best-effort optimisation, not a critical path.
fn maybe_spawn_openrouter_cache_refresh(api_key: Option<String>, base_url: String) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return; // No runtime active - skip (e.g. unit tests, sync callers).
    };
    let Some(key) = api_key else {
        return; // No key - cannot authenticate the request.
    };
    if !catalog::is_cache_stale("openrouter") {
        return; // Fresh disk cache - no need to refresh.
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

#[cfg(test)]
mod tests {
    use super::api_key::{key_file_for, read_key_from_json_file};
    use super::*;
    use sven_model::{get_driver, list_drivers};

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
        // error - the provider must always be recognised.
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
        // missing-key error - the provider must always be recognised.
        match from_config(&cfg) {
            Ok(_) => {}
            Err(e) => assert!(
                e.to_string().contains("API key"),
                "unexpected error (provider should be recognized): {e}"
            ),
        }
    }

    /// `provider: dbus` selects brain's D-Bus transport. It must never fall
    /// through to the OpenAI-compatible catch-all, which either refuses it
    /// for want of a base URL or - given one - silently speaks HTTP instead.
    #[cfg(all(unix, feature = "dbus"))]
    #[test]
    fn from_config_dbus_builds_the_dbus_provider() {
        let cfg = minimal_config("dbus", "brain/qwen3");
        let provider = from_config(&cfg).expect("the dbus provider is built without a base URL");
        assert_eq!(provider.name(), "dbus");
        assert_eq!(provider.model_name(), "brain/qwen3");
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

    // ── brain driver + keys file discovery ──────────────────────────────────
    //
    // Serializes env-var-touching tests: std::env::set_var mutates global
    // process state, which races under cargo test's default parallelism.

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn unique_temp_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "sven-model-test-{tag}-{}-{n}.json",
            std::process::id()
        ))
    }

    #[test]
    fn brain_driver_is_registered_with_correct_defaults() {
        let meta = get_driver("brain").expect("brain must be registered");
        assert_eq!(meta.default_base_url, Some("http://127.0.0.1:8788/v1"));
        assert_eq!(meta.default_api_key_env, Some("BRAIN_API_KEY"));
        assert!(meta.requires_api_key);
    }

    #[test]
    fn from_config_brain_uses_registry_defaults() {
        // Explicit key sidesteps env/file discovery entirely - this test is
        // only about the driver construction (base_url + auth), not discovery.
        let cfg = ModelConfig {
            provider: "brain".into(),
            name: "Qwen/Qwen3-0.6B".into(),
            api_key: Some("sk-brain-test".into()),
            ..ModelConfig::default()
        };
        let provider = from_config(&cfg).expect("brain must be a recognised driver");
        assert_eq!(provider.model_name(), "Qwen/Qwen3-0.6B");
    }

    // read_key_from_json_file: pure, path-parameterised - no env vars, no races.

    #[test]
    fn read_key_from_json_file_finds_the_openai_dialect_key() {
        let path = unique_temp_path("keys-ok");
        std::fs::write(
            &path,
            r#"{"openai":"sk-brain-abc123","anthropic":"sk-brain-def456"}"#,
        )
        .unwrap();
        let key = read_key_from_json_file(&path, "openai");
        let _ = std::fs::remove_file(&path);
        assert_eq!(key.as_deref(), Some("sk-brain-abc123"));
    }

    #[test]
    fn read_key_from_json_file_missing_dialect_is_none() {
        let path = unique_temp_path("keys-missing-dialect");
        std::fs::write(&path, r#"{"anthropic":"sk-brain-def456"}"#).unwrap();
        let key = read_key_from_json_file(&path, "openai");
        let _ = std::fs::remove_file(&path);
        assert_eq!(key, None);
    }

    #[test]
    fn read_key_from_json_file_missing_file_is_none_not_error() {
        let path = unique_temp_path("keys-does-not-exist");
        assert_eq!(read_key_from_json_file(&path, "openai"), None);
    }

    #[test]
    fn read_key_from_json_file_malformed_json_is_none_not_panic() {
        let path = unique_temp_path("keys-malformed");
        std::fs::write(&path, "{ not json").unwrap();
        let key = read_key_from_json_file(&path, "openai");
        let _ = std::fs::remove_file(&path);
        assert_eq!(key, None);
    }

    // key_file_for / resolve_api_key end-to-end: env-var precedence, locked.

    #[test]
    fn key_file_for_non_brain_provider_is_none() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(key_file_for("openai"), None);
    }

    #[test]
    fn resolve_api_key_falls_back_to_brain_keys_file_when_no_env_key() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let path = unique_temp_path("resolve-precedence");
        std::fs::write(&path, r#"{"openai":"sk-brain-from-file"}"#).unwrap();

        let prior = std::env::var("BRAIN_API_KEYS_FILE").ok();
        let prior_key_env = std::env::var("BRAIN_API_KEY").ok();
        // SAFETY (test-only, single-threaded via ENV_MUTEX): remove_var/set_var
        // are documented as unsound under concurrent access from other threads;
        // the mutex above is exactly what makes this call-site safe.
        unsafe {
            std::env::remove_var("BRAIN_API_KEY");
            std::env::set_var("BRAIN_API_KEYS_FILE", &path);
        }

        let cfg = ModelConfig {
            provider: "brain".into(),
            name: "Qwen/Qwen3-0.6B".into(),
            ..ModelConfig::default()
        };
        let key = resolve_api_key(&cfg);

        unsafe {
            match prior {
                Some(v) => std::env::set_var("BRAIN_API_KEYS_FILE", v),
                None => std::env::remove_var("BRAIN_API_KEYS_FILE"),
            }
            match prior_key_env {
                Some(v) => std::env::set_var("BRAIN_API_KEY", v),
                None => std::env::remove_var("BRAIN_API_KEY"),
            }
        }
        let _ = std::fs::remove_file(&path);

        assert_eq!(key.as_deref(), Some("sk-brain-from-file"));
    }

    #[test]
    fn resolve_api_key_prefers_env_var_over_keys_file() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let path = unique_temp_path("resolve-precedence-env-wins");
        std::fs::write(&path, r#"{"openai":"sk-brain-from-file"}"#).unwrap();

        let prior = std::env::var("BRAIN_API_KEYS_FILE").ok();
        let prior_key_env = std::env::var("BRAIN_API_KEY").ok();
        unsafe {
            std::env::set_var("BRAIN_API_KEY", "sk-brain-from-env");
            std::env::set_var("BRAIN_API_KEYS_FILE", &path);
        }

        let cfg = ModelConfig {
            provider: "brain".into(),
            name: "Qwen/Qwen3-0.6B".into(),
            ..ModelConfig::default()
        };
        let key = resolve_api_key(&cfg);

        unsafe {
            match prior {
                Some(v) => std::env::set_var("BRAIN_API_KEYS_FILE", v),
                None => std::env::remove_var("BRAIN_API_KEYS_FILE"),
            }
            match prior_key_env {
                Some(v) => std::env::set_var("BRAIN_API_KEY", v),
                None => std::env::remove_var("BRAIN_API_KEY"),
            }
        }
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            key.as_deref(),
            Some("sk-brain-from-env"),
            "an explicit env var must win over the keys file"
        );
    }

    #[test]
    fn all_registry_drivers_have_constructors() {
        // Every driver id in the registry must be handled by from_config
        // without returning "unknown provider" (API key errors are OK).
        for meta in list_drivers() {
            if meta.id == "litellm" || meta.id == "cloudflare" {
                // These require base_url - skip here.
                continue;
            }
            if meta.id == "azure" {
                // Azure requires resource name - skip.
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
}
