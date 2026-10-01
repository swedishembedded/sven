// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The provider section of the configuration file: the active model
//! (`model:`) and the named endpoints a model can be reached through
//! (`providers:`).
//!
//! Owned here because this crate is what reads it: [`crate::from_config`]
//! constructs a driver from a [`ModelConfig`], and [`crate::ModelResolver`]
//! turns a model string into one. The section's keys ([`ModelConfig::schema`],
//! [`providers_schema`]), its defaults, how an unconfigured model is detected
//! from the environment and how a named endpoint is expanded ([`settle`]) all
//! live beside the types.
//!
//! Swedish Embedded AB implements model-provider integration for its clients.
//! If your team needs expertise in connecting agents to hosted and local
//! language models then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sven_config::{default_true, Schema};
use tracing::{debug, warn};

/// The named endpoints of the `providers:` section, by name.
pub type Providers = HashMap<String, ProviderEntry>;

/// The keys of the `providers:` section: an entry per endpoint, named by the
/// user.
#[must_use]
pub fn providers_schema() -> Schema {
    Schema::entries(ProviderEntry::schema())
}

/// Per-model parameter overrides nested under a [`ProviderEntry`].
///
/// All fields are optional; absent fields inherit from the provider-level
/// defaults defined in [`ProviderEntry`], which in turn fall back to the
/// [`ModelConfig`] defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelParams {
    /// Total context window in tokens (input + output combined).
    ///
    /// When set, this value is used for session compaction decisions.
    /// If `max_output_tokens` is not set, this also caps the per-request
    /// output token limit (backward-compatible behaviour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Maximum output tokens per completion request.
    ///
    /// Sent to the provider API as the output token limit.
    /// When set alongside `max_tokens`, the constraint
    /// `max_tokens >= max_input_tokens + max_output_tokens` must hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Maximum input tokens allowed before compaction is forced.
    ///
    /// Optional cap on the input side of the context window.
    /// When set alongside `max_tokens`, the constraint
    /// `max_tokens >= max_input_tokens + max_output_tokens` must hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u32>,
    /// Sampling temperature (0.0-2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Free-form provider-specific options forwarded as-is to the driver.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub driver_options: serde_json::Value,
    /// Override cache_system_prompt for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_system_prompt: Option<bool>,
    /// Override extended_cache_time for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_cache_time: Option<bool>,
    /// Override cache_tools for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tools: Option<bool>,
    /// Override cache_conversation for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_conversation: Option<bool>,
    /// Override cache_images for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_images: Option<bool>,
    /// Override cache_tool_results for this model only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tool_results: Option<bool>,
    /// Path to YAML mock-responses file (used when driver = "mock")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock_responses_file: Option<String>,
    /// Override the accepted input modalities for this model only.
    /// Any of `text`, `image`, `audio`.  See [`ModelConfig::input_modalities`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
}

impl ModelParams {
    /// The keys of a `providers.<name>.models.<model>` entry.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "max_tokens",
            "max_output_tokens",
            "max_input_tokens",
            "temperature",
            "driver_options",
            "cache_system_prompt",
            "extended_cache_time",
            "cache_tools",
            "cache_conversation",
            "cache_images",
            "cache_tool_results",
            "mock_responses_file",
            "input_modalities",
        ])
    }
}

/// A named provider entry in the `providers` config section.
///
/// Represents a single API endpoint (e.g. a local LLM server, a cloud
/// provider account) together with all the models available on that endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEntry {
    /// Driver identifier that speaks this endpoint's protocol.
    /// Run `sven list-providers` for the full list.
    /// Examples: "openai" | "anthropic" | "google" | "ollama" | "vllm"
    pub name: String,

    /// Base URL override for this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Environment variable that holds the API key for this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Explicit API key; prefer `api_key_env` to keep secrets out of
    /// version-controlled config files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Models available on this endpoint.
    /// Keys are model names; values hold per-model parameter overrides that
    /// take precedence over the provider-level defaults below.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub models: std::collections::HashMap<String, ModelParams>,

    // ── Provider-level defaults (inherited by all models unless overridden) ──
    /// Default max_tokens for all models on this provider (can be overridden per-model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Default temperature for all models on this provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Default driver options for all models on this provider
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub driver_options: serde_json::Value,

    // ── Azure OpenAI ─────────────────────────────────────────────────────────
    /// Azure resource name (the subdomain of `.openai.azure.com`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_resource: Option<String>,
    /// Azure deployment name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_deployment: Option<String>,
    /// Azure REST API version string, e.g. `"2024-02-01"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_api_version: Option<String>,

    // ── AWS Bedrock ───────────────────────────────────────────────────────────
    /// AWS region override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,

    // ── Mock provider ─────────────────────────────────────────────────────────
    /// Path to YAML mock-responses file (used when name = "mock").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock_responses_file: Option<String>,

    // ── Multimodal capability declaration ────────────────────────────────────
    /// Default accepted input modalities for all models on this endpoint.
    /// Any of `text`, `image`, `audio`.  See [`ModelConfig::input_modalities`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
}

impl Default for ProviderEntry {
    fn default() -> Self {
        Self {
            name: "openai".into(),
            base_url: None,
            api_key_env: None,
            api_key: None,
            models: std::collections::HashMap::new(),
            max_tokens: None,
            temperature: None,
            driver_options: serde_json::Value::Null,
            azure_resource: None,
            azure_deployment: None,
            azure_api_version: None,
            aws_region: None,
            mock_responses_file: None,
            input_modalities: None,
        }
    }
}

impl ProviderEntry {
    /// The keys of a `providers.<name>` entry.
    ///
    /// `max_output_tokens` and `max_input_tokens` are accepted here although
    /// only a model entry acts on them, as they always have been.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "name",
            "base_url",
            "api_key_env",
            "api_key",
            "max_tokens",
            "max_output_tokens",
            "max_input_tokens",
            "temperature",
            "driver_options",
            "azure_resource",
            "azure_deployment",
            "azure_api_version",
            "aws_region",
            "mock_responses_file",
            "input_modalities",
        ])
        .with("models", Schema::entries(ModelParams::schema()))
    }

    /// Build a [`ModelConfig`] for the given `model_name` by merging:
    /// provider-level defaults → per-model overrides.
    #[must_use]
    pub fn to_model_config(&self, model_name: &str) -> ModelConfig {
        let mut cfg = ModelConfig {
            provider: self.name.clone(),
            name: model_name.to_string(),
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env.clone(),
            api_key: self.api_key.clone(),
            max_tokens: self.max_tokens,
            max_output_tokens: None,
            max_input_tokens: None,
            temperature: self.temperature,
            driver_options: self.driver_options.clone(),
            azure_resource: self.azure_resource.clone(),
            azure_deployment: self.azure_deployment.clone(),
            azure_api_version: self.azure_api_version.clone(),
            aws_region: self.aws_region.clone(),
            mock_responses_file: self.mock_responses_file.clone(),
            input_modalities: self.input_modalities.clone(),
            ..ModelConfig::default()
        };

        // Per-model overrides take precedence over provider-level defaults.
        if let Some(params) = self.models.get(model_name) {
            if let Some(v) = params.max_tokens {
                cfg.max_tokens = Some(v);
            }
            if let Some(v) = params.max_output_tokens {
                cfg.max_output_tokens = Some(v);
            }
            if let Some(v) = params.max_input_tokens {
                cfg.max_input_tokens = Some(v);
            }
            if let Some(v) = params.temperature {
                cfg.temperature = Some(v);
            }
            if !params.driver_options.is_null() {
                cfg.driver_options = params.driver_options.clone();
            }
            if let Some(v) = params.cache_system_prompt {
                cfg.cache_system_prompt = v;
            }
            if let Some(v) = params.extended_cache_time {
                cfg.extended_cache_time = v;
            }
            if let Some(v) = params.cache_tools {
                cfg.cache_tools = v;
            }
            if let Some(v) = params.cache_conversation {
                cfg.cache_conversation = v;
            }
            if let Some(v) = params.cache_images {
                cfg.cache_images = v;
            }
            if let Some(v) = params.cache_tool_results {
                cfg.cache_tool_results = v;
            }
            if let Some(ref f) = params.mock_responses_file {
                cfg.mock_responses_file = Some(f.clone());
            }
            if let Some(ref m) = params.input_modalities {
                cfg.input_modalities = Some(m.clone());
            }
        }

        cfg
    }
}

/// The active model: which driver, which model, how it is reached and what
/// it is allowed - the `model:` section, and what every driver is built from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Provider identifier.  Run `sven list-providers` for the full list.
    /// Common values: "openai" | "anthropic" | "google" | "azure" | "aws" |
    /// "groq" | "openrouter" | "ollama" | "mistral" | "deepseek" | "mock"
    pub provider: String,
    /// Model name forwarded to the provider API
    pub name: String,
    /// Environment variable that holds the API key (read at runtime)
    pub api_key_env: Option<String>,
    /// Explicit API key; prefer api_key_env in config files to avoid secrets
    /// in version-controlled files
    pub api_key: Option<String>,
    /// Base URL override.  Useful for local proxies, LiteLLM, or Cloudflare.
    /// For most hosted providers the correct default is auto-selected.
    pub base_url: Option<String>,
    /// Total context window in tokens (input + output combined).
    ///
    /// When set, this value is used for session compaction decisions.
    /// If `max_output_tokens` is not set, this also acts as the per-request
    /// output token limit for backward compatibility with older configs.
    pub max_tokens: Option<u32>,
    /// Maximum output tokens per completion request.
    ///
    /// Sent to the provider API as the output token limit (`max_tokens` or
    /// `max_completion_tokens` depending on the provider).  When set together
    /// with `max_tokens`, the constraint
    /// `max_tokens >= max_input_tokens + max_output_tokens` must hold.
    pub max_output_tokens: Option<u32>,
    /// Maximum input tokens before compaction is forced.
    ///
    /// Optional cap on the input side of the context window used by the
    /// compaction budget logic.  When set together with `max_tokens`, the
    /// constraint `max_tokens >= max_input_tokens + max_output_tokens` must hold.
    pub max_input_tokens: Option<u32>,
    /// Sampling temperature (0.0-2.0)
    pub temperature: Option<f32>,

    // ── Azure OpenAI ─────────────────────────────────────────────────────────
    /// Azure resource name (the subdomain of `.openai.azure.com`).
    /// Required when provider = "azure" and base_url is not set.
    pub azure_resource: Option<String>,
    /// Azure deployment name.  Defaults to `model.name` when not set.
    pub azure_deployment: Option<String>,
    /// Azure REST API version string, e.g. `"2024-02-01"`.
    pub azure_api_version: Option<String>,

    // ── AWS Bedrock ───────────────────────────────────────────────────────────
    /// AWS region override (also honoured via AWS_DEFAULT_REGION env var).
    pub aws_region: Option<String>,

    // ── Prompt caching ────────────────────────────────────────────────────────
    /// Attach an explicit cache-control marker to the system message.
    ///
    /// **Anthropic**: adds `"cache_control": {"type": "ephemeral"}` to the
    /// system block, which tells the API to cache the prefix up to and
    /// including that block.  Anthropic charges a one-time write fee and
    /// subsequent calls save ~90% on cached input tokens.
    ///
    /// **Other providers**: OpenAI and Google cache automatically; this flag
    /// has no effect for those providers.
    #[serde(default = "default_true")]
    pub cache_system_prompt: bool,

    /// Use the extended (1-hour) cache TTL instead of the default 5-minute
    /// window.  Applies to the system prompt (when `cache_system_prompt = true`)
    /// and to tool definitions (when `cache_tools = true`).  Only meaningful
    /// for the Anthropic provider.  Sends the
    /// `anthropic-beta: extended-cache-ttl-2025-04-11` header automatically.
    ///
    /// Conversation caching (`cache_conversation`) always uses the 5-minute
    /// TTL regardless of this setting, because conversation turns are
    /// typically frequent enough to keep the cache refreshed within 5 minutes.
    #[serde(default)]
    pub extended_cache_time: bool,

    /// Cache tool definitions using Anthropic prompt caching.
    ///
    /// Tool definitions are stable across requests within a session, making
    /// them ideal for caching.  The last tool in the list receives a
    /// `cache_control` marker so Anthropic caches all tool definitions as a
    /// prefix.  Uses the same TTL as `extended_cache_time` controls (1-hour
    /// when true, 5-minute otherwise).
    ///
    /// With many tools (each ~200-500 tokens), this can save thousands of
    /// tokens per request.
    #[serde(default = "default_true")]
    pub cache_tools: bool,

    /// Enable automatic conversation caching (Anthropic only).
    ///
    /// Adds a top-level `cache_control` marker that instructs Anthropic to
    /// automatically cache conversation history up to the last message.
    /// Subsequent turns read prior context from cache at 10% of the base
    /// token cost, dramatically reducing cost for multi-turn agent sessions.
    ///
    /// The cache breakpoint automatically advances with each new turn so no
    /// manual management is needed.
    #[serde(default = "default_true")]
    pub cache_conversation: bool,

    /// Cache image content blocks in conversation history (Anthropic only).
    ///
    /// Images are token-expensive: even a modest screenshot costs hundreds of
    /// input tokens every turn it remains in context.  Marking the oldest image
    /// blocks with `cache_control` preserves them across turns, saving ~90% on
    /// those tokens for the rest of the session.
    ///
    /// Uses the same TTL tier as `extended_cache_time` controls.  The number
    /// of cached images is bounded by the remaining Anthropic breakpoint budget
    /// (maximum 4 breakpoints total across system, tools, conversation, and
    /// images/tool-results).
    #[serde(default = "default_true")]
    pub cache_images: bool,

    /// Cache large tool results in conversation history (Anthropic only).
    ///
    /// When an agent reads files, runs commands, or fetches documents, those
    /// tool results can consume thousands of tokens on every subsequent turn.
    /// Marking them with `cache_control` once saves ~90% on those tokens for
    /// all following turns.
    ///
    /// A result is eligible when its serialised content exceeds 4 096
    /// characters (~1 024 tokens, the Anthropic minimum cacheable length for
    /// Sonnet-class models).  The oldest eligible results are cached first;
    /// the count is bounded by the remaining breakpoint budget.
    ///
    /// Uses the same TTL tier as `extended_cache_time` controls.
    #[serde(default = "default_true")]
    pub cache_tool_results: bool,

    // ── Provider-specific extras ──────────────────────────────────────────────
    /// Free-form provider-specific options forwarded as-is to the driver.
    /// Useful for headers or parameters not covered by the standard fields.
    #[serde(default)]
    pub driver_options: serde_json::Value,

    // ── Mock provider ─────────────────────────────────────────────────────────
    /// Path to YAML mock-responses file (used when provider = "mock").
    /// Can also be set via the SVEN_MOCK_RESPONSES environment variable.
    pub mock_responses_file: Option<String>,

    // ── Multimodal capability declaration ────────────────────────────────────
    /// Input modalities this model accepts: any of `text`, `image`, `audio`.
    ///
    /// The bundled static model catalog only knows about public models, so
    /// self-hosted multimodal endpoints must declare their capabilities here.
    /// When set, this overrides whatever the driver/catalog would report; when
    /// absent, the driver's own answer is used.
    ///
    /// ```yaml
    /// providers:
    ///   brain_openai:
    ///     name: openai
    ///     base_url: http://127.0.0.1:8788/v1
    ///     input_modalities: [text, image, audio]
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".into(),
            name: "openrouter/auto".into(),
            // api_key_env is intentionally None here.  resolve_api_key() falls
            // through to the driver registry, which already knows the canonical
            // env-var name for each provider (OPENAI_API_KEY, ANTHROPIC_API_KEY,
            // etc.).  Hard-coding it here would shadow the registry lookup and
            // cause the wrong key to be sent whenever the provider is overridden
            // at the step level (e.g. <!-- sven: provider=anthropic -->).
            api_key_env: None,
            api_key: None,
            base_url: None,
            max_tokens: None,
            max_output_tokens: None,
            max_input_tokens: None,
            temperature: Some(0.2),
            azure_resource: None,
            azure_deployment: None,
            azure_api_version: None,
            aws_region: None,
            // Comprehensive caching is on by default for every provider that
            // supports it (currently Anthropic).  The flags are no-ops for
            // providers such as OpenAI that cache automatically.  Only the
            // extended (1-hour) TTL remains opt-in because it carries a 2×
            // write cost that is only worthwhile when turns are >5 min apart.
            cache_system_prompt: true,
            extended_cache_time: false,
            cache_tools: true,
            cache_conversation: true,
            cache_images: true,
            cache_tool_results: true,
            driver_options: serde_json::Value::Null,
            mock_responses_file: None,
            input_modalities: None,
        }
    }
}

impl ModelConfig {
    /// The keys of the `model:` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "provider",
            "name",
            "api_key_env",
            "api_key",
            "base_url",
            "max_tokens",
            "max_output_tokens",
            "max_input_tokens",
            "temperature",
            "azure_resource",
            "azure_deployment",
            "azure_api_version",
            "aws_region",
            "cache_system_prompt",
            "extended_cache_time",
            "cache_tools",
            "cache_conversation",
            "cache_images",
            "cache_tool_results",
            "driver_options",
            "mock_responses_file",
            "input_modalities",
        ])
    }
}

/// Settles the active model once the configuration is read, and warns about
/// limits that cannot hold.
///
/// With no `model:` section (`model_configured` false), the model is chosen
/// from what the environment offers: a locally served brain, then
/// OpenRouter, Anthropic and OpenAI keys, in that order. A `model.provider`
/// naming one of `providers` is then expanded into that endpoint's settings
/// with its per-model overrides.
pub fn settle(model: &mut ModelConfig, providers: &Providers, model_configured: bool) {
    if !model_configured {
        detect_model(model);
    }
    expand_named_provider(model, providers);
    validate_token_limits(model, providers);
}

/// `provider/model` naming the active model so that another sven process
/// loading the same config resolves the same endpoint, key and limits.
///
/// A named `providers:` entry is expanded at load time, after which
/// `model.provider` holds only the driver id; handing a child process
/// `driver/model` would resolve the driver's defaults and silently drop the
/// entry's `base_url` and key. So the alias whose expansion produced the
/// active model (same driver, endpoint and key source) is named instead;
/// with no such entry the driver id is the right name.
#[must_use]
pub fn model_reference(model: &ModelConfig, providers: &Providers) -> String {
    // Several identical entries would all resolve the same; take the
    // smallest name so the reference does not depend on map order.
    let alias = providers
        .iter()
        .filter(|(_, entry)| {
            entry.name == model.provider
                && entry.base_url == model.base_url
                && entry.api_key_env == model.api_key_env
                && entry.api_key == model.api_key
        })
        .map(|(alias, _)| alias.as_str())
        .min();
    format!("{}/{}", alias.unwrap_or(&model.provider), model.name)
}

/// Chooses the model when the configuration names none, from what the
/// environment offers.
///
/// Priority: brain (local) > OpenRouter > Anthropic > OpenAI. A locally
/// running brain wins over cloud providers when detectable - getting started
/// on a brain-enabled machine should need zero config - but ONLY when
/// detectable, so an existing cloud user (who has neither `BRAIN_API_KEY`
/// nor a brain keys file) sees no change at all. Everything here is a sync,
/// network-free check (env var / file existence); whether brain is actually
/// reachable is verified when the provider is constructed
/// ([`crate::from_config_probed`]), which fails loudly if it is not.
fn detect_model(model: &mut ModelConfig) {
    if brain_is_locally_detectable() {
        model.provider = "brain".into();
        // Empty is a deliberate sentinel, not an oversight: the actual
        // resident model is whatever brain is currently serving, and that
        // requires a network call to discover - this runs while the
        // configuration loads, which is sync and must stay that way.
        // `from_config_probed` resolves the sentinel by asking brain's
        // `/v1/models` when it has exactly one chat-capable entry; a user
        // who wants a specific resident model names it via `--model` or an
        // explicit `model:` block, which bypasses this detection entirely.
        model.name = String::new();
    } else if std::env::var("OPENROUTER_API_KEY").is_ok() {
        // Keep the defaults: provider="openrouter", name="openrouter/auto".
    } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        model.provider = "anthropic".into();
        model.name = "claude-sonnet-4-6".into();
    } else if std::env::var("OPENAI_API_KEY").is_ok() {
        model.provider = "openai".into();
        model.name = "gpt-5.2".into();
    }
    // If no key is available the defaults remain (openrouter/auto), and
    // from_config() will produce a clear error when actually invoked.
}

/// Is a locally running `brain` model server detectable without any network
/// I/O?
///
/// Deliberately sync and cheap (env var reads + one `fs::metadata` stat). This
/// is a HINT, not proof brain is actually reachable - a stale leftover keys
/// file with brain no longer running is harmless: constructing the provider
/// turns an unreachable brain into a loud, actionable error rather than a
/// silent hang, same as any other misconfigured provider.
///
/// Set `SVEN_DISABLE_BRAIN_AUTODETECT=1` to opt out entirely (e.g. CI
/// environments that happen to have a stale brain keys file lying around but
/// want cloud-provider defaults).
fn brain_is_locally_detectable() -> bool {
    if std::env::var("SVEN_DISABLE_BRAIN_AUTODETECT").is_ok() {
        return false;
    }
    if std::env::var("BRAIN_API_KEY").is_ok() {
        return true;
    }
    brain_keys_file().is_some_and(|p| p.is_file())
}

fn brain_keys_file() -> Option<PathBuf> {
    crate::api_key::brain_keys_file()
}

/// If `model.provider` names one of `providers`, replace `model` with that
/// endpoint's settings and any per-model overrides registered for
/// `model.name`. This is what makes the provider-first layout work.
fn expand_named_provider(model: &mut ModelConfig, providers: &Providers) {
    if let Some(entry) = providers.get(&model.provider) {
        debug!(
            provider = %model.provider,
            model = %model.name,
            driver = %entry.name,
            "expanding named provider config"
        );
        *model = entry.to_model_config(&model.name);
    }
}

/// Warns when a total context window is smaller than the input and output
/// limits it must hold, for the active model and every listed model.
fn validate_token_limits(model: &ModelConfig, providers: &Providers) {
    validate_model_params_token_limits(
        model.max_tokens,
        model.max_input_tokens,
        model.max_output_tokens,
        "model",
    );
    for (alias, entry) in providers {
        for (model_name, params) in &entry.models {
            let effective_max_tokens = params.max_tokens.or(entry.max_tokens);
            validate_model_params_token_limits(
                effective_max_tokens,
                params.max_input_tokens,
                params.max_output_tokens,
                &format!("providers.{alias}.models.{model_name}"),
            );
        }
    }
}

/// Warn when `max_tokens` (total context) is less than the sum of the
/// optional `max_input_tokens` and `max_output_tokens` limits.
///
/// The constraint is:
///   `max_tokens >= max_input_tokens + max_output_tokens`
fn validate_model_params_token_limits(
    max_tokens: Option<u32>,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    path: &str,
) {
    if let (Some(total), Some(input), Some(output)) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        let sum = input.saturating_add(output);
        if total < sum {
            warn!(
                path,
                max_tokens = total,
                max_input_tokens = input,
                max_output_tokens = output,
                sum_of_parts = sum,
                "max_tokens ({total}) is less than max_input_tokens + max_output_tokens \
                 ({input} + {output} = {sum}); total must be >= sum of its parts"
            );
        }
    } else if let (Some(total), None, Some(output)) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        if total < output {
            warn!(
                path,
                max_tokens = total,
                max_output_tokens = output,
                "max_tokens ({total}) is less than max_output_tokens ({output}); \
                 total context must be >= output limit"
            );
        }
    } else if let (Some(total), Some(input), None) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        if total < input {
            warn!(
                path,
                max_tokens = total,
                max_input_tokens = input,
                "max_tokens ({total}) is less than max_input_tokens ({input}); \
                 total context must be >= input limit"
            );
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
