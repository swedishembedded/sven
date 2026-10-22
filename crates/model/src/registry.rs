// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Driver registry: static metadata for every supported model provider.
//!
//! This module acts as the single source of truth for which provider IDs exist
//! and what their defaults are.  It does **not** contain construction logic -
//! that lives in `from_config` (`sven-model-drivers`).

/// Metadata describing a registered model driver.
#[derive(Debug, Clone)]
pub struct DriverMeta {
    /// Unique provider id used in `model.provider` config field (e.g. `"openai"`).
    pub id: &'static str,
    /// Human-readable display name (e.g. `"OpenAI"`).
    pub name: &'static str,
    /// One-line description shown by `sven list-providers`.
    pub description: &'static str,
    /// Default base URL when the user does not set `model.base_url` in config.
    /// `None` means the user must supply a `base_url` (e.g. Azure, LiteLLM, Cloudflare).
    pub default_base_url: Option<&'static str>,
    /// Whether an explicit API key is required.
    pub requires_api_key: bool,
    /// Whether the server runs on the user's own hardware. A local server
    /// can legitimately take minutes to answer (prefill of a long prompt on
    /// modest hardware), so its stall limits are far more generous.
    pub local: bool,
}

/// Complete registry of supported drivers.
pub static DRIVERS: &[DriverMeta] = &[
    // ── Major cloud providers ─────────────────────────────────────────────────
    DriverMeta {
        id: "openai",
        name: "OpenAI",
        description: "OpenAI GPT and o-series models",
        default_base_url: Some("https://api.openai.com/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "anthropic",
        name: "Anthropic",
        description: "Anthropic Claude models",
        default_base_url: Some("https://api.anthropic.com"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "google",
        name: "Google Gemini",
        description: "Google Gemini models via Generative Language API",
        default_base_url: Some("https://generativelanguage.googleapis.com"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "azure",
        name: "Azure OpenAI",
        description: "Azure-hosted OpenAI deployments (set base_url or azure_* config fields)",
        default_base_url: None,
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "aws",
        name: "AWS Bedrock",
        description: "AWS Bedrock Converse API (uses AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
    },
    DriverMeta {
        id: "cohere",
        name: "Cohere",
        description: "Cohere Command models",
        default_base_url: Some("https://api.cohere.com"),
        requires_api_key: true,
        local: false,
    },
    // ── Gateways ──────────────────────────────────────────────────────────────
    DriverMeta {
        id: "openrouter",
        name: "OpenRouter",
        description: "OpenRouter gateway (200+ models from many providers)",
        default_base_url: Some("https://openrouter.ai/api/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "litellm",
        name: "LiteLLM",
        description: "LiteLLM proxy gateway (set base_url to your server)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
    },
    DriverMeta {
        id: "portkey",
        name: "Portkey",
        description: "Portkey AI gateway and observability platform",
        default_base_url: Some("https://api.portkey.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "vercel",
        name: "Vercel AI Gateway",
        description: "Vercel AI SDK gateway",
        default_base_url: Some("https://sdk.vercel.ai/openai"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "cloudflare",
        name: "Cloudflare AI Gateway",
        description: "Cloudflare AI Gateway (set base_url to your account-specific URL)",
        default_base_url: None,
        requires_api_key: true,
        local: false,
    },
    // ── Fast inference platforms ───────────────────────────────────────────────
    DriverMeta {
        id: "groq",
        name: "Groq",
        description: "Groq LPU fast inference",
        default_base_url: Some("https://api.groq.com/openai/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "cerebras",
        name: "Cerebras",
        description: "Cerebras fast inference platform",
        default_base_url: Some("https://api.cerebras.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    // ── Open model platforms ───────────────────────────────────────────────────
    DriverMeta {
        id: "together",
        name: "Together AI",
        description: "Together AI open model hosting platform",
        default_base_url: Some("https://api.together.xyz/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "fireworks",
        name: "Fireworks AI",
        description: "Fireworks AI fast open model inference",
        default_base_url: Some("https://api.fireworks.ai/inference/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "deepinfra",
        name: "DeepInfra",
        description: "DeepInfra open model hosting",
        default_base_url: Some("https://api.deepinfra.com/v1/openai"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "nebius",
        name: "Nebius AI",
        description: "Nebius AI model platform",
        default_base_url: Some("https://api.studio.nebius.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "sambanova",
        name: "SambaNova",
        description: "SambaNova fast inference",
        default_base_url: Some("https://api.sambanova.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "huggingface",
        name: "Hugging Face",
        description: "Hugging Face Inference Router",
        default_base_url: Some("https://router.huggingface.co/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "nvidia",
        name: "NVIDIA NIM",
        description: "NVIDIA NIM inference platform",
        default_base_url: Some("https://integrate.api.nvidia.com/v1"),
        requires_api_key: true,
        local: false,
    },
    // ── Specialized ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "perplexity",
        name: "Perplexity",
        description: "Perplexity AI online search and reasoning models",
        default_base_url: Some("https://api.perplexity.ai"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "mistral",
        name: "Mistral AI",
        description: "Mistral AI models including Codestral",
        default_base_url: Some("https://api.mistral.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "xai",
        name: "xAI",
        description: "xAI Grok models",
        default_base_url: Some("https://api.x.ai/v1"),
        requires_api_key: true,
        local: false,
    },
    // ── Regional providers ────────────────────────────────────────────────────
    DriverMeta {
        id: "deepseek",
        name: "DeepSeek",
        description: "DeepSeek reasoning and coder models",
        default_base_url: Some("https://api.deepseek.com/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "moonshot",
        name: "Moonshot AI",
        description: "Moonshot AI Kimi models",
        default_base_url: Some("https://api.moonshot.cn/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "dashscope",
        name: "Qwen/DashScope",
        description: "Alibaba Qwen models via DashScope compatible API",
        default_base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "glm",
        name: "GLM/Z.AI",
        description: "Zhipu AI GLM models",
        default_base_url: Some("https://open.bigmodel.cn/api/paas/v4"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "minimax",
        name: "MiniMax",
        description: "MiniMax AI models",
        default_base_url: Some("https://api.minimax.chat/v1"),
        requires_api_key: true,
        local: false,
    },
    DriverMeta {
        id: "qianfan",
        name: "Baidu Qianfan",
        description: "Baidu Qianfan LLM platform",
        default_base_url: Some("https://qianfan.baidubce.com/v2"),
        requires_api_key: true,
        local: false,
    },
    // ── Local / OSS ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "sven",
        name: "Sven (custom local server)",
        description: "User-defined OpenAI-compatible local server; set base_url in config",
        default_base_url: None,
        requires_api_key: false,
        local: true,
    },
    DriverMeta {
        id: "brain",
        name: "Brain (local edge-AI server)",
        description: "brain serve on this machine; found from the connection record brain publishes, with its address and key",
        default_base_url: Some("http://127.0.0.1:8788/v1"),
        requires_api_key: true,
        local: true,
    },
    DriverMeta {
        id: "ollama",
        name: "Ollama",
        description: "Ollama local model runner (http://localhost:11434)",
        default_base_url: Some("http://localhost:11434/v1"),
        requires_api_key: false,
        local: true,
    },
    DriverMeta {
        id: "vllm",
        name: "vLLM",
        description: "vLLM local inference server (http://localhost:8000)",
        default_base_url: Some("http://localhost:8000/v1"),
        requires_api_key: false,
        local: true,
    },
    DriverMeta {
        id: "lmstudio",
        name: "LM Studio",
        description: "LM Studio local model server (http://localhost:1234)",
        default_base_url: Some("http://localhost:1234/v1"),
        requires_api_key: false,
        local: true,
    },
    // ── Local IPC ─────────────────────────────────────────────────────────────
    DriverMeta {
        id: "dbus",
        name: "D-Bus (brain)",
        description: "Local brain server over D-Bus (com.swedishembedded.Brain1); \
                      carries image and audio blobs as file descriptors",
        // Deliberately None: this driver has no HTTP endpoint at all, and it
        // must never fall through to the OpenAI-compat catch-all's base-URL
        // logic.  `from_config` handles "dbus" explicitly before that arm.
        default_base_url: None,
        requires_api_key: false,
        local: true,
    },
    // ── Testing ───────────────────────────────────────────────────────────────
    DriverMeta {
        id: "mock",
        name: "Mock",
        description: "Mock driver for tests (no network, echoes input)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
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
    fn servers_on_the_users_own_hardware_are_marked_local() {
        let mut local: Vec<_> = DRIVERS.iter().filter(|d| d.local).map(|d| d.id).collect();
        local.sort_unstable();
        assert_eq!(
            local,
            ["brain", "dbus", "lmstudio", "ollama", "sven", "vllm"]
        );
    }

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
