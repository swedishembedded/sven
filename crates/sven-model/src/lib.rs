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
        ))),
        "anthropic" => Ok(Box::new(AnthropicProvider::with_cache(
            cfg.name.clone(),
            key(),
            cfg.base_url.clone(),
            resolved_max_tokens,
            cfg.temperature,
            cfg.cache_system_prompt,
            cfg.extended_cache_time,
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
}
