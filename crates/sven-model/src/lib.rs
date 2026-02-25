// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
pub mod catalog;
pub mod registry;
pub mod sanitize;
pub(crate) mod openai_compat;
mod types;
mod provider;
mod openai;
mod anthropic;
mod google;
mod aws;
mod cohere;
mod mock;
mod yaml_mock;

pub use catalog::{ModelCatalogEntry, InputModality};
pub use types::*;
pub use provider::ModelProvider;
pub use openai::OpenAiProvider;
pub use anthropic::AnthropicProvider;
pub use mock::{MockProvider, ScriptedMockProvider};
pub use yaml_mock::YamlMockProvider;
pub use registry::{DriverMeta, get_driver, list_drivers};

use anyhow::bail;
use openai_compat::{AuthStyle, OpenAICompatProvider};
use sven_config::ModelConfig;

/// Construct a boxed [`ModelProvider`] from configuration.
///
/// Selects the driver implementation based on `cfg.provider`.  Run
/// `sven list-providers` to see all recognised provider ids.
///
/// When `max_tokens` is not set in config, the model's `max_output_tokens` is
/// resolved from the static catalog.  If the model is not found there a safe
/// default of 4096 is used.
pub fn from_config(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    // key() returns a fresh Option<String> on each call so that each match arm
    // can take ownership without cross-arm borrow issues.
    let key = || resolve_api_key(cfg);
    let resolved_max_tokens = cfg.max_tokens.or_else(|| {
        catalog::lookup(&cfg.provider, &cfg.name)
            .map(|e| e.max_output_tokens)
    });

    // Helper that reads `base_url` from config or falls back to a static default.
    let base_url = |default: &str| -> String {
        cfg.base_url.clone().unwrap_or_else(|| default.into())
    };

    match cfg.provider.as_str() {
        // ── Native drivers ────────────────────────────────────────────────────
        "openai" => Ok(Box::new(OpenAiProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
            cfg.driver_options.clone(),
        ))),
        "anthropic" => Ok(Box::new(AnthropicProvider::with_cache(
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
        ))),
        "google" => Ok(Box::new(google::GoogleProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
        ))),
        "aws" => Ok(Box::new(aws::BedrockProvider::new(
            cfg.name.clone(),
            cfg.aws_region.clone(),
            resolved_max_tokens,
            cfg.temperature,
        ))),
        "cohere" => Ok(Box::new(cohere::CohereProvider::new(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
        ))),

        // ── Azure OpenAI (OpenAI-compat with special URL + api-key header) ────
        "azure" => {
            let chat_url = if let Some(b) = &cfg.base_url {
                let api_ver = cfg.azure_api_version.as_deref().unwrap_or("2024-02-01");
                format!("{}/chat/completions?api-version={}", b.trim_end_matches('/'), api_ver)
            } else {
                let resource = cfg.azure_resource.as_deref().unwrap_or("myresource");
                let deployment = cfg.azure_deployment.as_deref().unwrap_or(&cfg.name);
                let api_ver = cfg.azure_api_version.as_deref().unwrap_or("2024-02-01");
                format!(
                    "https://{resource}.openai.azure.com/openai/deployments/{deployment}/chat/completions?api-version={api_ver}"
                )
            };
            Ok(Box::new(OpenAICompatProvider::with_full_chat_url(
                "azure",
                cfg.name.clone(),
                key(),
                chat_url,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                openai_compat::AuthStyle::ApiKeyHeader,
                cfg.driver_options.clone(),
            )))
        }

        // ── OpenAI-compatible gateways ────────────────────────────────────────
        "openrouter" => Ok(Box::new(OpenAICompatProvider::new(
            "openrouter",
            cfg.name.clone(),
            key(),
            &base_url("https://openrouter.ai/api/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![
                ("HTTP-Referer".into(), "https://github.com/svenai/sven".into()),
                ("X-Title".into(), "sven".into()),
            ],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "litellm" => {
            let b = cfg.base_url.as_deref()
                .ok_or_else(|| anyhow::anyhow!("litellm provider requires base_url in config"))?;
            Ok(Box::new(OpenAICompatProvider::new(
                "litellm",
                cfg.name.clone(),
                key(),
                b,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                AuthStyle::Bearer,
                cfg.driver_options.clone(),
            )))
        }
        "portkey" => Ok(Box::new(OpenAICompatProvider::new(
            "portkey",
            cfg.name.clone(),
            key(),
            &base_url("https://api.portkey.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            // Portkey virtual key can be passed via driver_options.portkey_virtual_key
            portkey_extra_headers(cfg),
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "vercel" => Ok(Box::new(OpenAICompatProvider::new(
            "vercel",
            cfg.name.clone(),
            key(),
            &base_url("https://sdk.vercel.ai/openai"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "cloudflare" => {
            let b = cfg.base_url.as_deref()
                .ok_or_else(|| anyhow::anyhow!("cloudflare provider requires base_url in config (account-specific URL)"))?;
            Ok(Box::new(OpenAICompatProvider::new(
                "cloudflare",
                cfg.name.clone(),
                key(),
                b,
                resolved_max_tokens,
                cfg.temperature,
                vec![],
                AuthStyle::Bearer,
                cfg.driver_options.clone(),
            )))
        }

        // ── Fast inference ────────────────────────────────────────────────────
        "groq" => Ok(Box::new(OpenAICompatProvider::new(
            "groq",
            cfg.name.clone(),
            key(),
            &base_url("https://api.groq.com/openai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "cerebras" => Ok(Box::new(OpenAICompatProvider::new(
            "cerebras",
            cfg.name.clone(),
            key(),
            &base_url("https://api.cerebras.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),

        // ── Open model platforms ──────────────────────────────────────────────
        "together" => Ok(Box::new(OpenAICompatProvider::new(
            "together",
            cfg.name.clone(),
            key(),
            &base_url("https://api.together.xyz/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "fireworks" => Ok(Box::new(OpenAICompatProvider::new(
            "fireworks",
            cfg.name.clone(),
            key(),
            &base_url("https://api.fireworks.ai/inference/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "deepinfra" => Ok(Box::new(OpenAICompatProvider::new(
            "deepinfra",
            cfg.name.clone(),
            key(),
            &base_url("https://api.deepinfra.com/v1/openai"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "nebius" => Ok(Box::new(OpenAICompatProvider::new(
            "nebius",
            cfg.name.clone(),
            key(),
            &base_url("https://api.studio.nebius.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "sambanova" => Ok(Box::new(OpenAICompatProvider::new(
            "sambanova",
            cfg.name.clone(),
            key(),
            &base_url("https://api.sambanova.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "huggingface" => Ok(Box::new(OpenAICompatProvider::new(
            "huggingface",
            cfg.name.clone(),
            key(),
            &base_url("https://router.huggingface.co/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "nvidia" => Ok(Box::new(OpenAICompatProvider::new(
            "nvidia",
            cfg.name.clone(),
            key(),
            &base_url("https://integrate.api.nvidia.com/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),

        // ── Specialized ───────────────────────────────────────────────────────
        "perplexity" => Ok(Box::new(OpenAICompatProvider::new(
            "perplexity",
            cfg.name.clone(),
            key(),
            &base_url("https://api.perplexity.ai"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "mistral" => Ok(Box::new(OpenAICompatProvider::new(
            "mistral",
            cfg.name.clone(),
            key(),
            &base_url("https://api.mistral.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "xai" => Ok(Box::new(OpenAICompatProvider::new(
            "xai",
            cfg.name.clone(),
            key(),
            &base_url("https://api.x.ai/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),

        // ── Regional providers ────────────────────────────────────────────────
        "deepseek" => Ok(Box::new(OpenAICompatProvider::new(
            "deepseek",
            cfg.name.clone(),
            key(),
            &base_url("https://api.deepseek.com/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "moonshot" => Ok(Box::new(OpenAICompatProvider::new(
            "moonshot",
            cfg.name.clone(),
            key(),
            &base_url("https://api.moonshot.cn/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "dashscope" => Ok(Box::new(OpenAICompatProvider::new(
            "dashscope",
            cfg.name.clone(),
            key(),
            &base_url("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "glm" => Ok(Box::new(OpenAICompatProvider::new(
            "glm",
            cfg.name.clone(),
            key(),
            &base_url("https://open.bigmodel.cn/api/paas/v4"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "minimax" => Ok(Box::new(OpenAICompatProvider::new(
            "minimax",
            cfg.name.clone(),
            key(),
            &base_url("https://api.minimax.chat/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),
        "qianfan" => Ok(Box::new(OpenAICompatProvider::new(
            "qianfan",
            cfg.name.clone(),
            key(),
            &base_url("https://qianfan.baidubce.com/v2"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::Bearer,
            cfg.driver_options.clone(),
        ))),

        // ── Local / OSS ───────────────────────────────────────────────────────
        "ollama" => Ok(Box::new(OpenAICompatProvider::new(
            "ollama",
            cfg.name.clone(),
            None, // no key needed
            &base_url("http://localhost:11434/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::None,
            cfg.driver_options.clone(),
        ))),
        "vllm" => Ok(Box::new(OpenAICompatProvider::new(
            "vllm",
            cfg.name.clone(),
            key(),
            &base_url("http://localhost:8000/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            // vLLM accepts an optional bearer token
            if key().is_some() { AuthStyle::Bearer } else { AuthStyle::None },
            cfg.driver_options.clone(),
        ))),
        "lmstudio" => Ok(Box::new(OpenAICompatProvider::new(
            "lmstudio",
            cfg.name.clone(),
            None,
            &base_url("http://localhost:1234/v1"),
            resolved_max_tokens,
            cfg.temperature,
            vec![],
            AuthStyle::None,
            cfg.driver_options.clone(),
        ))),

        // ── Testing / Mock ────────────────────────────────────────────────────
        "mock" => {
            let responses_path = std::env::var("SVEN_MOCK_RESPONSES").ok()
                .or_else(|| cfg.mock_responses_file.clone());
            if let Some(path) = responses_path {
                Ok(Box::new(YamlMockProvider::from_file(&path)?))
            } else {
                Ok(Box::new(MockProvider))
            }
        }

        other => {
            let known: Vec<&str> = registry::known_driver_ids().collect();
            bail!(
                "unknown model provider: {other:?}\n\
                 Run `sven list-providers` for a full list, or check your config.\n\
                 Known providers: {known}",
                known = known.join(", ")
            )
        }
    }
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

fn portkey_extra_headers(cfg: &ModelConfig) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(vk) = cfg.driver_options.get("portkey_virtual_key").and_then(|v| v.as_str()) {
        headers.push(("x-portkey-virtual-key".into(), vk.to_string()));
    }
    headers
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
pub fn resolve_model_from_config(
    config: &sven_config::Config,
    override_str: &str,
) -> ModelConfig {
    let (provider_key, model_suffix) =
        if let Some((p, m)) = override_str.split_once('/') {
            (p, Some(m))
        } else {
            (override_str, None)
        };

    // Named custom provider in config.providers takes precedence.
    if let Some(named) = config.providers.get(provider_key) {
        let mut cfg = named.clone();
        if let Some(model) = model_suffix {
            cfg.name = model.to_string();
        }
        return cfg;
    }

    // Smart catalog lookup: start from a clean default ModelConfig whenever
    // the requested model is found in the static catalog.  This prevents
    // custom base_url / api_key values from leaking across providers when the
    // user's config.model points at a local/custom endpoint.
    //
    // Two forms are handled:
    //   "gpt-4o"          — bare model name, no provider prefix
    //   "openai/gpt-4o"   — explicit provider/model from a known driver
    //
    // In both cases, if the model is in the catalog we start from a fresh
    // ModelConfig (provider defaults, no base_url) and only inherit
    // credentials from config.model when the provider matches.
    let catalog_entry = if let Some(model_name) = model_suffix {
        // "provider/model" form — look up by provider+name in catalog.
        // Only apply catalog defaults when provider_key is a known driver
        // (not a custom provider alias that wasn't caught above).
        if get_driver(provider_key).is_some() {
            catalog::lookup(provider_key, model_name)
        } else {
            None
        }
    } else if get_driver(override_str).is_none() {
        // Bare model name (not a provider id) — look up by model name alone.
        catalog::lookup_by_model_name(override_str)
    } else {
        None
    };

    if let Some(entry) = catalog_entry {
        let mut cfg = ModelConfig {
            provider: entry.provider.clone(),
            name: entry.id.clone(),
            ..ModelConfig::default()
        };
        // Preserve api_key credentials when the resolved provider matches
        // the config's provider (same service, different model).
        if cfg.provider == config.model.provider {
            cfg.api_key = config.model.api_key.clone();
            cfg.api_key_env = config.model.api_key_env.clone();
        }
        return cfg;
    }

    // Fall back to standard resolution with config.model as base.
    // This path handles provider ids without catalog entries (e.g. a bare
    // "anthropic" to switch provider while keeping the current model name)
    // and any custom-endpoint models not listed in the catalog.
    resolve_model_cfg(&config.model, override_str)
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
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_anthropic_succeeds() {
        let cfg = minimal_config("anthropic", "claude-opus-4-5");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_google_succeeds() {
        let cfg = minimal_config("google", "gemini-2.0-flash-exp");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_mock_succeeds() {
        let cfg = minimal_config("mock", "mock-model");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_groq_succeeds() {
        let cfg = minimal_config("groq", "llama-3.3-70b-versatile");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_ollama_requires_no_key() {
        let cfg = minimal_config("ollama", "llama3.2");
        assert!(from_config(&cfg).is_ok());
    }

    #[test]
    fn from_config_deepseek_succeeds() {
        let cfg = minimal_config("deepseek", "deepseek-chat");
        assert!(from_config(&cfg).is_ok());
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
        assert!(cfg.api_key_env.is_none(), "key env must be cleared when provider changes");
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn resolve_bare_model_name_keeps_provider() {
        let cfg = resolve_model_cfg(&openai_base(), "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o-mini");
        assert_eq!(cfg.api_key_env.as_deref(), Some("OPENAI_API_KEY"),
            "key env must be preserved when provider does not change");
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
        assert_eq!(cfg.api_key_env.as_deref(), Some("OPENAI_API_KEY"),
            "key env must not be cleared when provider is unchanged");
    }

    // ── resolve_model_from_config ─────────────────────────────────────────────

    fn config_with_named_provider() -> sven_config::Config {
        use std::collections::HashMap;
        let mut providers = HashMap::new();
        providers.insert("my_ollama".into(), ModelConfig {
            provider: "openai".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            name: "llama3.2".into(),
            api_key: Some("ollama".into()),
            ..ModelConfig::default()
        });
        sven_config::Config {
            providers,
            ..sven_config::Config::default()
        }
    }

    #[test]
    fn resolve_from_config_named_provider_used_as_base() {
        let config = config_with_named_provider();
        let cfg = resolve_model_from_config(&config, "my_ollama");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "llama3.2");
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434/v1"));
    }

    #[test]
    fn resolve_from_config_named_provider_with_model_override() {
        let config = config_with_named_provider();
        let cfg = resolve_model_from_config(&config, "my_ollama/codellama");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "codellama");
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434/v1"),
            "base_url from named provider must be kept");
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
        assert_eq!(cfg.provider, "openai", "provider must be openai (from catalog)");
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
        assert!(cfg.api_key.is_none(), "OpenAI api_key must not leak to anthropic config");
    }
}
