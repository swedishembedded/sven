// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Concrete [`sven_model::ModelProvider`] drivers (OpenAI, Anthropic, Google,
//! AWS Bedrock, Cohere, D-Bus and the shared OpenAI-compatible wire format
//! every other provider speaks), the `from_config`/`from_config_probed`
//! factory that selects and constructs one from a [`ModelConfig`], and the
//! resolution of a user-supplied model string ([`ModelResolver`],
//! [`resolve_model_from_config`]). The provider section of the configuration
//! file - [`ModelConfig`], [`ProviderEntry`] and how an unconfigured model is
//! detected - is here too, in [`config`], because this crate reads it.
//!
//! `reqwest`, SigV4 signing and every provider's dependency closure live here,
//! so a crate that needs only the trait and its types pays for none of it.
/// Sampling temperature a driver sends when neither the request nor the
/// model's configuration sets one.
pub(crate) const DEFAULT_TEMPERATURE: f32 = 0.2;

mod anthropic;
mod api_key;
mod aws;
mod block_turns;
pub mod brain_local;
mod cohere;
pub mod config;
pub mod credentials;
pub mod dbus;
mod discovery;
mod embedding;
mod google;
mod openai;
pub(crate) mod openai_compat;
mod openrouter;
mod provider_check;
pub mod reachable;
mod resolve;
mod thinking;

pub use anthropic::AnthropicProvider;
pub use config::{providers_schema, ModelConfig, ModelParams, ProviderEntry, Providers};
pub use embedding::OpenAiEmbedder;
pub use openai::OpenAiProvider;
pub use provider_check::{check_provider, ProviderCheck, ProviderReport, Verdict};
pub use resolve::{resolve_model_cfg, resolve_model_from_config, ModelResolver};

use anyhow::bail;
use api_key::resolve_api_key;
use async_trait::async_trait;
use futures::Stream;
use openai_compat::{AuthStyle, OpenAICompatProvider};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
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
/// A provider that needs a key and has none (see [`resolve_api_key`] for
/// where one is looked for) fails here with what to do, instead of letting
/// the first request surface an opaque 401.
fn check_api_key_requirement(cfg: &ModelConfig) -> anyhow::Result<()> {
    if cfg.api_key.is_some() {
        return Ok(());
    }
    if let Some(file) = &cfg.api_key_file {
        return api_key::read_key_file(file).map(|_| ());
    }
    let needs_key = registry::get_driver(&cfg.provider).is_some_and(|m| m.requires_api_key);
    if !needs_key || resolve_api_key(cfg).is_some() {
        return Ok(());
    }
    if cfg.provider == "brain" {
        bail!(
            "No running brain server was found for model '{}': brain publishes its address and key \
             when it serves (in {}).\nStart one with `brain serve`, or name another model with --model.",
            cfg.name,
            brain_local::runtime_dir().display(),
        );
    }
    bail!(
        "No API key found for provider '{}' (model '{}').\n\
         Store one with `sven auth set {}` (it then serves every {} model), or name a key file \
         in the configuration as `api_key_file`. Keys are not read from the environment.",
        cfg.provider,
        cfg.name,
        cfg.provider,
        cfg.provider,
    );
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
    inner: Arc<dyn ModelProvider>,
    /// Total context window from `cfg.max_tokens` (when `max_output_tokens` is
    /// also set, `max_tokens` is interpreted as the pure total context limit).
    config_context_window: Option<u32>,
    /// Resolved output token cap: `cfg.max_output_tokens` if set, else
    /// `cfg.max_tokens` for backward compatibility.
    max_output_tokens: Option<u32>,
    /// Input modalities declared in config (`cfg.input_modalities`).
    ///
    /// The bundled catalog cannot know about self-hosted multimodal models, so
    /// a config declaration is authoritative when present.  `None` means "no
    /// declaration" and the inner driver's answer is used instead.
    input_modalities: Option<Vec<sven_model::catalog::InputModality>>,
    /// What the live server last reported; `None` when the provider was built
    /// without probing ([`from_config`]).
    live: Option<Arc<discovery::LiveState>>,
}

impl ConfigBoundedProvider {
    /// The context window in force: the smaller of the configured and the
    /// server-reported one. A probe only ever narrows a hand-written number.
    fn effective_context_window(&self) -> Option<u32> {
        let probed = self.live.as_ref().and_then(|l| l.context_window());
        match (self.config_context_window, probed) {
            (Some(c), Some(p)) => Some(c.min(p)),
            (c, p) => c.or(p),
        }
    }
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

/// The `host[:port]` of `url`, for naming a server in an error.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

#[async_trait]
impl ModelProvider for ConfigBoundedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn endpoint_host(&self) -> Option<String> {
        self.inner.endpoint_host()
    }

    /// Forwards to the driver. On a local server, a stale probe is refreshed
    /// in the background, and a request rejected as too large for the window
    /// re-probes before the error is returned, so the retry sees the new one.
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>>>
    {
        let Some(live) = self.live.as_ref().filter(|l| l.is_refreshable()) else {
            return self.inner.complete(req).await;
        };
        live.refresh_if_stale(&self.inner);
        let result = self.inner.complete(req).await;
        if let Err(e) = &result {
            if discovery::is_context_overflow(&format!("{e:#}")) {
                live.refresh(&*self.inner).await;
            }
        }
        result
    }

    /// Re-probes the live server now, updating the context window and the
    /// reported adapter, and returns what it serves.
    async fn discover(&self) -> Option<sven_model::ServedModels> {
        match self.live.as_ref().filter(|l| l.is_refreshable()) {
            Some(live) => live.refresh(&*self.inner).await,
            None => self.inner.discover().await,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<sven_model::ModelCatalogEntry>> {
        self.inner.list_models().await
    }

    /// Configured cap, else the server's, else the catalog's.
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
            .or_else(|| self.live.as_ref().and_then(|l| l.max_output_tokens()))
            .or_else(|| self.inner.catalog_max_output_tokens())
    }

    fn take_model_change(&self) -> Option<sven_model::ModelChange> {
        self.live.as_ref()?.take_change()
    }

    fn tool_support(&self) -> Option<bool> {
        self.live.as_ref().and_then(|l| l.tool_support())
    }

    fn supports_tool_choice_none(&self) -> bool {
        self.inner.supports_tool_choice_none()
    }

    fn catalog_context_window(&self) -> Option<u32> {
        self.effective_context_window()
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
        self.effective_context_window()
    }

    fn config_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }
}

// ── from_config ───────────────────────────────────────────────────────────────

/// Select and construct the driver implementation for `cfg.provider`, plus
/// the two config-derived limits [`from_config`]/[`from_config_probed`] wrap
/// it with. Shared by both so neither duplicates the driver-selection match
/// or double-wraps the result in `ConfigBoundedProvider`.
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
    // A brain server reads `enable_thinking` from the chat template's keyword
    // arguments: carry the setting there, explicitly, rather than leave the
    // served template's default to decide whenever the configuration says.
    let with_thinking;
    let cfg = if cfg.provider == "brain" && cfg.enable_thinking.is_some() {
        with_thinking = cfg.with_thinking_in_driver_options();
        &with_thinking
    } else {
        cfg
    };
    // A brain named by driver alone is the server running here: its address
    // comes from the connection record it publishes.
    let with_brain;
    let cfg = match (cfg.provider.as_str(), &cfg.base_url) {
        ("brain", None) => match discovery::effective_base_url(cfg) {
            Some(url) => {
                with_brain = ModelConfig {
                    base_url: Some(url),
                    ..cfg.clone()
                };
                &with_brain
            }
            None => cfg,
        },
        _ => cfg,
    };
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
            openrouter::maybe_spawn_openrouter_cache_refresh(key(), or_base.clone());
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
                openrouter::transform_openrouter_options(cfg),
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
        "dbus" => dbus::build(cfg, resolved_max_tokens, cfg.temperature)?,

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
/// `build_inner` for the output-token resolution priority order.
///
/// Wraps the driver in `ConfigBoundedProvider` so that
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
        inner: Arc::from(inner),
        config_context_window: context_window,
        max_output_tokens,
        input_modalities: cfg
            .input_modalities
            .as_deref()
            .and_then(parse_input_modalities),
        live: None,
    }))
}

/// Like [`from_config`], but additionally asks the live server what it is
/// serving (see [`ModelProvider::discover`]), so no model name has to be
/// written down for a local server.
///
/// 0. A brain that was only *detected* (see `ModelConfig::detection_fallback`)
///    and does not answer is replaced by the hosted model a stored key
///    implies, so a server that stopped answering strands no one.
/// 1. Nothing configured (the built-in default with no key for it anywhere):
///    the first of the default local endpoints that answers (brain, ollama,
///    vllm, lmstudio, probed together within one second) is used, with the
///    model it serves.
/// 2. An EMPTY model name on a local OpenAI-compatible server (the sentinel
///    the configuration loader writes when it detects brain) resolves to the
///    served model: the only one, or else the first chat-capable one with an
///    info line. A named model is never replaced, and hosted providers are
///    never asked.
/// 3. The exposed `catalog_context_window()` is clamped to
///    `min(config, probed)` - never widened, only narrowed to what the server
///    can actually serve. With no probe answer the config value (or none) is
///    kept, identical to `from_config`.
///
/// On a local server the answer is kept fresh: a stale probe is refreshed in
/// the background on the next turn, and a context-length-exceeded error
/// re-probes before it is returned. A changed adapter digest is logged.
///
/// Every probe is bounded (about two seconds each) and best-effort; an
/// unreachable server never fails or delays the build beyond that.
pub async fn from_config_probed(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    let cfg = &resolve_probed(cfg).await;
    let (inner, config_context_window, max_output_tokens) = build_inner(cfg)?;
    let inner: Arc<dyn ModelProvider> = Arc::from(inner);
    let local =
        discovery::effective_base_url(cfg).is_some_and(|u| discovery::is_local_endpoint(&u));
    let live = discovery::LiveState::new(&cfg.name, local);
    live.refresh(&*inner).await;
    Ok(Box::new(ConfigBoundedProvider {
        inner,
        config_context_window,
        max_output_tokens,
        input_modalities: cfg
            .input_modalities
            .as_deref()
            .and_then(parse_input_modalities),
        live: Some(live),
    }))
}

/// Steps 0-2 of [`from_config_probed`]: the configuration a session actually
/// runs with, once a detected brain, an unconfigured start and an empty model
/// name are settled against what answers.
pub(crate) async fn resolve_probed(cfg: &ModelConfig) -> ModelConfig {
    let cfg = discovery::resolve_detected_brain(cfg).await;
    let cfg = discovery::resolve_unconfigured(&cfg).await;
    discovery::resolve_empty_model_name(&cfg).await
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

/// Serializes every test of this crate that points process-global state
/// (`XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, provider key variables) elsewhere:
/// `cargo test` runs tests on parallel threads of one process, and two locks
/// over the same variables do not exclude each other.
#[cfg(test)]
pub(crate) static TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{get_driver, list_drivers};

    #[test]
    fn url_host_names_the_server_with_its_port() {
        assert_eq!(
            url_host("http://127.0.0.1:8788/v1/chat/completions").as_deref(),
            Some("127.0.0.1:8788")
        );
        assert_eq!(
            url_host("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(url_host("not a url"), None);
    }

    #[test]
    fn a_wrapped_driver_reports_its_server_and_locality() {
        let local = from_config(&ModelConfig {
            provider: "ollama".into(),
            name: "m".into(),
            ..ModelConfig::default()
        })
        .unwrap();
        assert_eq!(local.endpoint_host().as_deref(), Some("localhost:11434"));
        assert!(local.is_local());

        let hosted = from_config(&ModelConfig {
            provider: "openai".into(),
            name: "m".into(),
            api_key: Some("k".into()),
            ..ModelConfig::default()
        })
        .unwrap();
        assert_eq!(hosted.endpoint_host().as_deref(), Some("api.openai.com"));
        assert!(!hosted.is_local());
    }

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
    #[cfg(all(target_os = "linux", feature = "dbus"))]
    #[test]
    fn from_config_dbus_builds_the_dbus_provider() {
        let cfg = minimal_config("dbus", "brain/qwen3");
        let provider = from_config(&cfg).expect("the dbus provider is built without a base URL");
        assert_eq!(provider.name(), "dbus");
        assert_eq!(provider.model_name(), "brain/qwen3");
    }

    /// A build without the transport still knows the provider by name and
    /// says why it cannot have it.
    #[cfg(not(all(target_os = "linux", feature = "dbus")))]
    #[test]
    fn from_config_dbus_without_the_transport_is_refused_by_name() {
        let cfg = minimal_config("dbus", "brain/qwen3");
        let Err(err) = from_config(&cfg) else {
            panic!("a build without the `dbus` feature must not build a dbus provider");
        };
        assert!(
            err.to_string()
                .contains("'dbus' model provider is not available in this build"),
            "{err}"
        );
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
            api_key_file: Some("/nonexistent/key".into()),
            ..ModelConfig::default()
        };
        let key = resolve_api_key(&cfg);
        assert_eq!(key.as_deref(), Some("explicit-key"));
    }

    // ── key resolution ─────────────────────────────────────────────────────

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sven-model-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Run `f` with the user's configuration directory at `dir`.
    fn with_config_dir<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY (test-only, serialized by crate::TEST_ENV).
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir) };
        let out = f();
        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        out
    }

    #[test]
    fn brain_driver_is_registered_with_correct_defaults() {
        let meta = get_driver("brain").expect("brain must be registered");
        assert_eq!(meta.default_base_url, Some("http://127.0.0.1:8788/v1"));
        assert!(meta.requires_api_key);
    }

    #[test]
    fn from_config_brain_uses_registry_defaults() {
        // Explicit key sidesteps discovery entirely - this test is only about
        // the driver construction (base_url + auth), not discovery.
        let cfg = ModelConfig {
            provider: "brain".into(),
            name: "Qwen/Qwen3-0.6B".into(),
            api_key: Some("sk-brain-test".into()),
            ..ModelConfig::default()
        };
        let provider = from_config(&cfg).expect("brain must be a recognised driver");
        assert_eq!(provider.model_name(), "Qwen/Qwen3-0.6B");
    }

    /// A key file holds the key; a missing one is an error naming it.
    #[test]
    fn a_configured_key_file_is_the_key() {
        let dir = scratch("key-file");
        let file = dir.join("key");
        std::fs::write(&file, "  sk-from-file\n").unwrap();
        let cfg = ModelConfig {
            provider: "openrouter".into(),
            api_key_file: Some(file.to_string_lossy().into_owned()),
            ..ModelConfig::default()
        };
        assert_eq!(resolve_api_key(&cfg).as_deref(), Some("sk-from-file"));
        let missing = ModelConfig {
            api_key_file: Some(dir.join("absent").to_string_lossy().into_owned()),
            ..cfg
        };
        let e = from_config(&missing)
            .err()
            .expect("a missing key file")
            .to_string();
        assert!(e.contains("absent"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stored key serves its driver; the environment's variables name none.
    #[test]
    fn the_stored_key_of_a_driver_is_used_and_the_environment_is_not() {
        let dir = scratch("stored");
        with_config_dir(&dir, || {
            // SAFETY (test-only, serialized by crate::TEST_ENV in with_config_dir).
            unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-from-environment") };
            let cfg = ModelConfig {
                provider: "openrouter".into(),
                name: "x/y".into(),
                ..ModelConfig::default()
            };
            assert_eq!(resolve_api_key(&cfg), None, "the environment names no key");
            let e = from_config(&cfg).err().expect("no key").to_string();
            assert!(
                e.contains("sven auth set openrouter") && !e.contains("export"),
                "{e}"
            );
            credentials::set("openrouter", "sk-stored").unwrap();
            assert_eq!(resolve_api_key(&cfg).as_deref(), Some("sk-stored"));
            let own = ModelConfig {
                base_url: Some("https://openrouter.ai/api/v1/".into()),
                ..cfg.clone()
            };
            assert_eq!(
                resolve_api_key(&own).as_deref(),
                Some("sk-stored"),
                "its own endpoint, however spelled"
            );
            let elsewhere = ModelConfig {
                base_url: Some("https://collector.example/v1".into()),
                ..cfg.clone()
            };
            assert_eq!(
                resolve_api_key(&elsewhere),
                None,
                "a stored key never follows a base_url elsewhere"
            );
            unsafe { std::env::remove_var("OPENROUTER_API_KEY") };
        });
        let _ = std::fs::remove_dir_all(&dir);
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
