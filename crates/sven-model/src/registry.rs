// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Driver registry: static metadata for every supported model provider.
//!
//! This module acts as the single source of truth for which provider IDs exist
//! and what their defaults are.  It does **not** contain construction logic –
//! that lives in [`crate::from_config`].

/// Metadata describing a registered model driver.
#[derive(Debug, Clone)]
pub struct DriverMeta {
    /// Unique provider id used in `model.provider` config field (e.g. `"openai"`).
    pub id: &'static str,
    /// Human-readable display name (e.g. `"OpenAI"`).
    pub name: &'static str,
    /// One-line description shown by `sven list-providers`.
    pub description: &'static str,
    /// Default environment variable that holds the API key (e.g. `"OPENAI_API_KEY"`).
    /// `None` for providers that require no key (local servers) or use non-key auth (AWS).
    pub default_api_key_env: Option<&'static str>,
    /// Default base URL when the user does not set `model.base_url` in config.
    /// `None` means the user must supply a `base_url` (e.g. Azure, LiteLLM, Cloudflare).
    pub default_base_url: Option<&'static str>,
    /// Whether an explicit API key is required.
    pub requires_api_key: bool,
}

/// Complete registry of supported drivers.
pub static DRIVERS: &[DriverMeta] = &[
    // ── Major cloud providers ─────────────────────────────────────────────────
    DriverMeta {
        id: "openai",
        name: "OpenAI",
        description: "OpenAI GPT and o-series models",
        default_api_key_env: Some("OPENAI_API_KEY"),
        default_base_url: Some("https://api.openai.com/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "anthropic",
        name: "Anthropic",
        description: "Anthropic Claude models",
        default_api_key_env: Some("ANTHROPIC_API_KEY"),
        default_base_url: Some("https://api.anthropic.com"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "google",
        name: "Google Gemini",
        description: "Google Gemini models via Generative Language API",
        default_api_key_env: Some("GEMINI_API_KEY"),
        default_base_url: Some("https://generativelanguage.googleapis.com"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "azure",
        name: "Azure OpenAI",
        description: "Azure-hosted OpenAI deployments (set base_url or azure_* config fields)",
        default_api_key_env: Some("AZURE_OPENAI_API_KEY"),
        default_base_url: None,
        requires_api_key: true,
    },
    DriverMeta {
        id: "aws",
        name: "AWS Bedrock",
        description: "AWS Bedrock Converse API (uses AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY)",
        default_api_key_env: None,
        default_base_url: None,
        requires_api_key: false,
    },
    DriverMeta {
        id: "cohere",
        name: "Cohere",
        description: "Cohere Command models",
        default_api_key_env: Some("COHERE_API_KEY"),
        default_base_url: Some("https://api.cohere.com"),
        requires_api_key: true,
    },
    // ── Gateways ──────────────────────────────────────────────────────────────
    DriverMeta {
        id: "openrouter",
        name: "OpenRouter",
        description: "OpenRouter gateway (200+ models from many providers)",
        default_api_key_env: Some("OPENROUTER_API_KEY"),
        default_base_url: Some("https://openrouter.ai/api/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "litellm",
        name: "LiteLLM",
        description: "LiteLLM proxy gateway (set base_url to your server)",
        default_api_key_env: Some("LITELLM_API_KEY"),
        default_base_url: None,
        requires_api_key: false,
    },
    DriverMeta {
        id: "portkey",
        name: "Portkey",
        description: "Portkey AI gateway and observability platform",
        default_api_key_env: Some("PORTKEY_API_KEY"),
        default_base_url: Some("https://api.portkey.ai/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "vercel",
        name: "Vercel AI Gateway",
        description: "Vercel AI SDK gateway",
        default_api_key_env: Some("VERCEL_API_KEY"),
        default_base_url: Some("https://sdk.vercel.ai/openai"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "cloudflare",
        name: "Cloudflare AI Gateway",
        description: "Cloudflare AI Gateway (set base_url to your account-specific URL)",
        default_api_key_env: Some("CLOUDFLARE_API_TOKEN"),
        default_base_url: None,
        requires_api_key: true,
    },
    // ── Fast inference platforms ───────────────────────────────────────────────
    DriverMeta {
        id: "groq",
        name: "Groq",
        description: "Groq LPU fast inference",
        default_api_key_env: Some("GROQ_API_KEY"),
        default_base_url: Some("https://api.groq.com/openai/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "cerebras",
        name: "Cerebras",
        description: "Cerebras fast inference platform",
        default_api_key_env: Some("CEREBRAS_API_KEY"),
        default_base_url: Some("https://api.cerebras.ai/v1"),
        requires_api_key: true,
    },
    // ── Open model platforms ───────────────────────────────────────────────────
    DriverMeta {
        id: "together",
        name: "Together AI",
        description: "Together AI open model hosting platform",
        default_api_key_env: Some("TOGETHER_API_KEY"),
        default_base_url: Some("https://api.together.xyz/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "fireworks",
        name: "Fireworks AI",
        description: "Fireworks AI fast open model inference",
        default_api_key_env: Some("FIREWORKS_API_KEY"),
        default_base_url: Some("https://api.fireworks.ai/inference/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "deepinfra",
        name: "DeepInfra",
        description: "DeepInfra open model hosting",
        default_api_key_env: Some("DEEPINFRA_API_KEY"),
        default_base_url: Some("https://api.deepinfra.com/v1/openai"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "nebius",
        name: "Nebius AI",
        description: "Nebius AI model platform",
        default_api_key_env: Some("NEBIUS_API_KEY"),
        default_base_url: Some("https://api.studio.nebius.ai/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "sambanova",
        name: "SambaNova",
        description: "SambaNova fast inference",
        default_api_key_env: Some("SAMBANOVA_API_KEY"),
        default_base_url: Some("https://api.sambanova.ai/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "huggingface",
        name: "Hugging Face",
        description: "Hugging Face Inference Router",
        default_api_key_env: Some("HF_API_KEY"),
        default_base_url: Some("https://router.huggingface.co/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "nvidia",
        name: "NVIDIA NIM",
        description: "NVIDIA NIM inference platform",
        default_api_key_env: Some("NVIDIA_API_KEY"),
        default_base_url: Some("https://integrate.api.nvidia.com/v1"),
        requires_api_key: true,
    },
    // ── Specialized ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "perplexity",
        name: "Perplexity",
        description: "Perplexity AI online search and reasoning models",
        default_api_key_env: Some("PERPLEXITY_API_KEY"),
        default_base_url: Some("https://api.perplexity.ai"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "mistral",
        name: "Mistral AI",
        description: "Mistral AI models including Codestral",
        default_api_key_env: Some("MISTRAL_API_KEY"),
        default_base_url: Some("https://api.mistral.ai/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "xai",
        name: "xAI",
        description: "xAI Grok models",
        default_api_key_env: Some("XAI_API_KEY"),
        default_base_url: Some("https://api.x.ai/v1"),
        requires_api_key: true,
    },
    // ── Regional providers ────────────────────────────────────────────────────
    DriverMeta {
        id: "deepseek",
        name: "DeepSeek",
        description: "DeepSeek reasoning and coder models",
        default_api_key_env: Some("DEEPSEEK_API_KEY"),
        default_base_url: Some("https://api.deepseek.com/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "moonshot",
        name: "Moonshot AI",
        description: "Moonshot AI Kimi models",
        default_api_key_env: Some("MOONSHOT_API_KEY"),
        default_base_url: Some("https://api.moonshot.cn/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "dashscope",
        name: "Qwen/DashScope",
        description: "Alibaba Qwen models via DashScope compatible API",
        default_api_key_env: Some("DASHSCOPE_API_KEY"),
        default_base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "glm",
        name: "GLM/Z.AI",
        description: "Zhipu AI GLM models",
        default_api_key_env: Some("GLM_API_KEY"),
        default_base_url: Some("https://open.bigmodel.cn/api/paas/v4"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "minimax",
        name: "MiniMax",
        description: "MiniMax AI models",
        default_api_key_env: Some("MINIMAX_API_KEY"),
        default_base_url: Some("https://api.minimax.chat/v1"),
        requires_api_key: true,
    },
    DriverMeta {
        id: "qianfan",
        name: "Baidu Qianfan",
        description: "Baidu Qianfan LLM platform",
        default_api_key_env: Some("QIANFAN_API_KEY"),
        default_base_url: Some("https://qianfan.baidubce.com/v2"),
        requires_api_key: true,
    },
    // ── Local / OSS ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "ollama",
        name: "Ollama",
        description: "Ollama local model runner (http://localhost:11434)",
        default_api_key_env: None,
        default_base_url: Some("http://localhost:11434/v1"),
        requires_api_key: false,
    },
    DriverMeta {
        id: "vllm",
        name: "vLLM",
        description: "vLLM local inference server (http://localhost:8000)",
        default_api_key_env: None,
        default_base_url: Some("http://localhost:8000/v1"),
        requires_api_key: false,
    },
    DriverMeta {
        id: "lmstudio",
        name: "LM Studio",
        description: "LM Studio local model server (http://localhost:1234)",
        default_api_key_env: None,
        default_base_url: Some("http://localhost:1234/v1"),
        requires_api_key: false,
    },
    // ── Testing ───────────────────────────────────────────────────────────────
    DriverMeta {
        id: "mock",
        name: "Mock",
        description: "Mock driver for tests (no network, echoes input)",
        default_api_key_env: None,
        default_base_url: None,
        requires_api_key: false,
    },
];

/// Returns all registered drivers in declaration order.
pub fn list_drivers() -> &'static [DriverMeta] {
    DRIVERS
}

/// Look up a driver by its id.  Returns `None` for unknown ids.
pub fn get_driver(id: &str) -> Option<&'static DriverMeta> {
    DRIVERS.iter().find(|d| d.id == id)
}

/// Returns an iterator over all known driver ids.
pub fn known_driver_ids() -> impl Iterator<Item = &'static str> {
    DRIVERS.iter().map(|d| d.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_non_empty() {
        assert!(!DRIVERS.is_empty());
    }

    #[test]
    fn all_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in DRIVERS {
            assert!(seen.insert(d.id), "duplicate driver id: {}", d.id);
        }
    }

    #[test]
    fn get_driver_returns_correct_entry() {
        let d = get_driver("openai").expect("openai must be registered");
        assert_eq!(d.name, "OpenAI");
    }

    #[test]
    fn get_driver_returns_none_for_unknown() {
        assert!(get_driver("totally-unknown-provider-xyz").is_none());
    }

    #[test]
    fn known_driver_ids_covers_major_providers() {
        let ids: Vec<&str> = known_driver_ids().collect();
        for required in &[
            "openai",
            "anthropic",
            "google",
            "aws",
            "azure",
            "groq",
            "ollama",
        ] {
            assert!(
                ids.contains(required),
                "missing required driver: {required}"
            );
        }
    }
}
