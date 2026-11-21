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

use serde::{Deserialize, Serialize};
use sven_config::{default_true, Schema};

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

    /// File holding the API key for this endpoint (`~/` is the home
    /// directory). Without it and `api_key`, the key stored for the driver
    /// (`sven auth set <driver>`) is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,

    /// Explicit API key; prefer `sven auth set` or `api_key_file` to keep
    /// secrets out of version-controlled config files.
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
            api_key_file: None,
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
            "api_key_file",
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
        .with("api_key_env", Schema::ignored(API_KEY_ENV_REMOVED))
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
            api_key_file: self.api_key_file.clone(),
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

/// The header naming the conversation a call is made for, so the service
/// records its cost under it. Here, not in `invoke`, because the chat drivers
/// send it too and they build for a browser, where `invoke` does not.
pub const SESSION_HEADER: &str = "x-sven-session";

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
    /// File holding the API key (`~/` is the home directory). Without it and
    /// `api_key`, the key stored for the provider (`sven auth set`) is used.
    pub api_key_file: Option<String>,
    /// Explicit API key; prefer `sven auth set` or `api_key_file` to keep
    /// secrets out of version-controlled files
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
    /// Whether a thinking model reasons before it answers, sent to a `brain`
    /// server as `chat_template_kwargs.enable_thinking`. Unset (the default)
    /// sends nothing and the served chat template decides. An explicit
    /// `driver_options.chat_template_kwargs.enable_thinking` wins over this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,

    // ── Mock provider ─────────────────────────────────────────────────────────
    /// Path to YAML mock-responses file (used when provider = "mock").
    /// Can also be set via the SVEN_MOCK_RESPONSES environment variable.
    pub mock_responses_file: Option<String>,

    /// The conversation this model's calls are made for, sent with each as
    /// `X-Sven-Session` so the sven service records its cost under it. Set
    /// in the process for a conversation of the service (`sven rc`, and the
    /// sub-agents it starts), never in a configuration file.
    #[serde(skip)]
    pub session: Option<String>,

    // ── Multimodal capability declaration ────────────────────────────────────
    /// Input modalities this model accepts: any of `text`, `image`, `audio`.
    ///
    /// A self-hosted endpoint often does not report what its model accepts,
    /// so it can declare its capabilities here.
    /// When set, this overrides whatever the driver/served catalog would report; when
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

    /// Send sampling controls the provider does not declare it honours (the run warns once and
    /// records them as unhonoured) instead of refusing the request, as by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sampling_best_effort: bool,

    /// The model to use instead when this one was auto-detected as a local
    /// brain and brain does not answer (see [`settle`]). Set only by
    /// detection, when a cloud key offers an alternative; never read from or
    /// written to a configuration file, so an explicit `model:` section is
    /// never second-guessed.
    #[serde(skip)]
    pub detection_fallback: Option<Box<ModelConfig>>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".into(),
            name: "openrouter/auto".into(),
            // No key source here: resolve_api_key() falls through to the key
            // stored for whichever provider this ends up naming, so a
            // provider overridden at the step level (e.g.
            // <!-- sven: provider=anthropic -->) gets its own key.
            api_key_file: None,
            api_key: None,
            base_url: None,
            max_tokens: None,
            max_output_tokens: None,
            max_input_tokens: None,
            temperature: None,
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
            enable_thinking: None,
            mock_responses_file: None,
            session: None,
            input_modalities: None,
            sampling_best_effort: false,
            detection_fallback: None,
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
            "api_key_file",
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
            "enable_thinking",
            "mock_responses_file",
            "input_modalities",
            "sampling_best_effort",
        ])
        .with("api_key_env", Schema::ignored(API_KEY_ENV_REMOVED))
    }
}

/// The `model:` section as the file writes it: a patch, every field `None`
/// unless the file sets it (an explicit `null` counts as unset). Decoded
/// beside [`ModelConfig`] so that what was set can be told apart from what
/// was left at its default, which a decoded [`ModelConfig`] cannot say: a
/// section that names a provider entry lays exactly what it sets over that
/// entry ([`Self::apply`]).
///
/// The fields are [`ModelConfig`]'s file keys, one for one (a test round-trips
/// a fully populated [`ModelConfig`] through this type).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSection {
    /// [`ModelConfig::provider`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// [`ModelConfig::name`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// [`ModelConfig::api_key_file`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
    /// [`ModelConfig::api_key`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// [`ModelConfig::base_url`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// [`ModelConfig::max_tokens`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// [`ModelConfig::max_output_tokens`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// [`ModelConfig::max_input_tokens`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u32>,
    /// [`ModelConfig::temperature`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// [`ModelConfig::azure_resource`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_resource: Option<String>,
    /// [`ModelConfig::azure_deployment`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_deployment: Option<String>,
    /// [`ModelConfig::azure_api_version`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azure_api_version: Option<String>,
    /// [`ModelConfig::aws_region`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
    /// [`ModelConfig::cache_system_prompt`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_system_prompt: Option<bool>,
    /// [`ModelConfig::extended_cache_time`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_cache_time: Option<bool>,
    /// [`ModelConfig::cache_tools`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tools: Option<bool>,
    /// [`ModelConfig::cache_conversation`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_conversation: Option<bool>,
    /// [`ModelConfig::cache_images`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_images: Option<bool>,
    /// [`ModelConfig::cache_tool_results`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tool_results: Option<bool>,
    /// [`ModelConfig::driver_options`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_options: Option<serde_json::Value>,
    /// [`ModelConfig::enable_thinking`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    /// [`ModelConfig::mock_responses_file`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock_responses_file: Option<String>,
    /// [`ModelConfig::input_modalities`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    /// [`ModelConfig::sampling_best_effort`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_best_effort: Option<bool>,
}

impl ModelSection {
    /// Lays every setting this section gives over `model`; a field the
    /// section leaves unset keeps `model`'s. `provider` and `name` select the
    /// endpoint rather than configure it and are not applied here.
    /// `driver_options` are merged key by key: an object's keys over
    /// `model`'s, any other value in their place.
    pub fn apply(&self, model: &mut ModelConfig) {
        // A set field replaces `model`'s: `T` itself, or `Some(T)` where
        // `model` holds an `Option<T>`.
        macro_rules! set {
            ($($field:ident),* $(,)?) => {$(
                if let Some(value) = &self.$field {
                    model.$field = value.clone().into();
                }
            )*};
        }
        set!(
            api_key_file,
            api_key,
            base_url,
            max_tokens,
            max_output_tokens,
            max_input_tokens,
            temperature,
            azure_resource,
            azure_deployment,
            azure_api_version,
            aws_region,
            cache_system_prompt,
            extended_cache_time,
            cache_tools,
            cache_conversation,
            cache_images,
            cache_tool_results,
            enable_thinking,
            mock_responses_file,
            input_modalities,
            sampling_best_effort,
        );
        if let Some(requested) = &self.driver_options {
            merge_driver_options(&mut model.driver_options, requested.clone());
        }
    }
}

/// Lays the options a `model:` section asked for over those of its endpoint,
/// key by key, so a request setting written next to `provider:` is not lost
/// to the expansion. A non-object value replaces the endpoint's options.
fn merge_driver_options(base: &mut serde_json::Value, requested: serde_json::Value) {
    match (base.as_object_mut(), requested) {
        (_, serde_json::Value::Null) => {}
        (Some(base), serde_json::Value::Object(requested)) => base.extend(requested),
        (_, requested) => *base = requested,
    }
}

/// Why a configuration's `api_key_env` does nothing, and what replaces it:
/// said where the key is read, so the run does not fail later on a missing
/// key with no hint.
const API_KEY_ENV_REMOVED: &str =
    "keys are no longer read from environment variables; name a file with `api_key_file` \
     or store the key with `sven auth set <provider>`";

#[path = "config_settle.rs"]
mod settle;
pub use settle::{model_reference, settle};

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
