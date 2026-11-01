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
    /// What the driver adds to the OpenAI-protocol requests it sends.
    /// Ignored by drivers that speak another protocol.
    pub requests: RequestExtras,
}

/// What one OpenAI-protocol gateway needs on top of the plain protocol, as
/// data: every such driver is the same client, configured by these.
#[derive(Debug, Clone, Copy)]
pub struct RequestExtras {
    /// Headers sent on every request.
    pub headers: &'static [(&'static str, &'static str)],
    /// Headers taken from the user's `driver_options`, as
    /// `(driver_options key, header name)`: the header is sent when the key
    /// holds a string.
    pub headers_from_options: &'static [(&'static str, &'static str)],
    /// Rewrites the user's `driver_options` into the body fields the gateway
    /// expects.
    pub rewrite_options: Option<fn(serde_json::Value) -> serde_json::Value>,
}

impl RequestExtras {
    /// The plain protocol: nothing added.
    pub const NONE: Self = Self {
        headers: &[],
        headers_from_options: &[],
        rewrite_options: None,
    };
}

/// Rewrite the `auto_router_allowed_models` convenience key of OpenRouter's
/// `driver_options` into the nested `plugins` structure its API expects:
///
/// ```yaml
/// driver_options:
///   auto_router_allowed_models: ["anthropic/*", "openai/gpt-5.1"]
/// ```
/// becomes `{ "plugins": [{ "id": "auto-router", "allowed_models": [...] }] }`.
/// A raw `plugins` key is passed through unchanged.
fn rewrite_openrouter_options(mut opts: serde_json::Value) -> serde_json::Value {
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
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "anthropic",
        name: "Anthropic",
        description: "Anthropic Claude models",
        default_base_url: Some("https://api.anthropic.com"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "google",
        name: "Google Gemini",
        description: "Google Gemini models via Generative Language API",
        default_base_url: Some("https://generativelanguage.googleapis.com"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "azure",
        name: "Azure OpenAI",
        description: "Azure-hosted OpenAI deployments (set base_url or azure_* config fields)",
        default_base_url: None,
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "aws",
        name: "AWS Bedrock",
        description: "AWS Bedrock Converse API (uses AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "cohere",
        name: "Cohere",
        description: "Cohere Command models",
        default_base_url: Some("https://api.cohere.com"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Gateways ──────────────────────────────────────────────────────────────
    DriverMeta {
        id: "openrouter",
        name: "OpenRouter",
        description: "OpenRouter gateway (200+ models from many providers)",
        default_base_url: Some("https://openrouter.ai/api/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras {
            headers: &[
                ("HTTP-Referer", "https://github.com/svenai/sven"),
                ("X-Title", "sven"),
            ],
            rewrite_options: Some(rewrite_openrouter_options),
            ..RequestExtras::NONE
        },
    },
    DriverMeta {
        id: "litellm",
        name: "LiteLLM",
        description: "LiteLLM proxy gateway (set base_url to your server)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "portkey",
        name: "Portkey",
        description: "Portkey AI gateway and observability platform",
        default_base_url: Some("https://api.portkey.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras {
            headers_from_options: &[("portkey_virtual_key", "x-portkey-virtual-key")],
            ..RequestExtras::NONE
        },
    },
    DriverMeta {
        id: "vercel",
        name: "Vercel AI Gateway",
        description: "Vercel AI SDK gateway",
        default_base_url: Some("https://sdk.vercel.ai/openai"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "cloudflare",
        name: "Cloudflare AI Gateway",
        description: "Cloudflare AI Gateway (set base_url to your account-specific URL)",
        default_base_url: None,
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Fast inference platforms ───────────────────────────────────────────────
    DriverMeta {
        id: "groq",
        name: "Groq",
        description: "Groq LPU fast inference",
        default_base_url: Some("https://api.groq.com/openai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "cerebras",
        name: "Cerebras",
        description: "Cerebras fast inference platform",
        default_base_url: Some("https://api.cerebras.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Open model platforms ───────────────────────────────────────────────────
    DriverMeta {
        id: "together",
        name: "Together AI",
        description: "Together AI open model hosting platform",
        default_base_url: Some("https://api.together.xyz/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "fireworks",
        name: "Fireworks AI",
        description: "Fireworks AI fast open model inference",
        default_base_url: Some("https://api.fireworks.ai/inference/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "deepinfra",
        name: "DeepInfra",
        description: "DeepInfra open model hosting",
        default_base_url: Some("https://api.deepinfra.com/v1/openai"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "nebius",
        name: "Nebius AI",
        description: "Nebius AI model platform",
        default_base_url: Some("https://api.studio.nebius.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "sambanova",
        name: "SambaNova",
        description: "SambaNova fast inference",
        default_base_url: Some("https://api.sambanova.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "huggingface",
        name: "Hugging Face",
        description: "Hugging Face Inference Router",
        default_base_url: Some("https://router.huggingface.co/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "nvidia",
        name: "NVIDIA NIM",
        description: "NVIDIA NIM inference platform",
        default_base_url: Some("https://integrate.api.nvidia.com/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Specialized ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "perplexity",
        name: "Perplexity",
        description: "Perplexity AI online search and reasoning models",
        default_base_url: Some("https://api.perplexity.ai"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "mistral",
        name: "Mistral AI",
        description: "Mistral AI models including Codestral",
        default_base_url: Some("https://api.mistral.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "xai",
        name: "xAI",
        description: "xAI Grok models",
        default_base_url: Some("https://api.x.ai/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Regional providers ────────────────────────────────────────────────────
    DriverMeta {
        id: "deepseek",
        name: "DeepSeek",
        description: "DeepSeek reasoning and coder models",
        default_base_url: Some("https://api.deepseek.com/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "moonshot",
        name: "Moonshot AI",
        description: "Moonshot AI Kimi models",
        default_base_url: Some("https://api.moonshot.cn/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "dashscope",
        name: "Qwen/DashScope",
        description: "Alibaba Qwen models via DashScope compatible API",
        default_base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "glm",
        name: "GLM/Z.AI",
        description: "Zhipu AI GLM models",
        default_base_url: Some("https://open.bigmodel.cn/api/paas/v4"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "minimax",
        name: "MiniMax",
        description: "MiniMax AI models",
        default_base_url: Some("https://api.minimax.chat/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "qianfan",
        name: "Baidu Qianfan",
        description: "Baidu Qianfan LLM platform",
        default_base_url: Some("https://qianfan.baidubce.com/v2"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "sven",
        name: "Sven",
        description: "The hosted sven service: its models, prices and web search, with the key `sven auth login` stores; base_url for another sven server",
        default_base_url: Some("https://sven.swedishembedded.com/v1"),
        requires_api_key: true,
        local: false,
        requests: RequestExtras::NONE,
    },
    // ── Local / OSS ───────────────────────────────────────────────────────────
    DriverMeta {
        id: "brain",
        name: "Brain (local edge-AI server)",
        description: "brain serve on this machine; found from the connection record brain publishes, with its address and key",
        default_base_url: Some("http://127.0.0.1:8788/v1"),
        requires_api_key: true,
        local: true,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "ollama",
        name: "Ollama",
        description: "Ollama local model runner (http://localhost:11434)",
        default_base_url: Some("http://localhost:11434/v1"),
        requires_api_key: false,
        local: true,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "vllm",
        name: "vLLM",
        description: "vLLM local inference server (http://localhost:8000)",
        default_base_url: Some("http://localhost:8000/v1"),
        requires_api_key: false,
        local: true,
        requests: RequestExtras::NONE,
    },
    DriverMeta {
        id: "lmstudio",
        name: "LM Studio",
        description: "LM Studio local model server (http://localhost:1234)",
        default_base_url: Some("http://localhost:1234/v1"),
        requires_api_key: false,
        local: true,
        requests: RequestExtras::NONE,
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
        requests: RequestExtras::NONE,
    },
    // ── Testing ───────────────────────────────────────────────────────────────
    DriverMeta {
        id: "mock",
        name: "Mock",
        description: "Mock driver for tests (no network, echoes input)",
        default_base_url: None,
        requires_api_key: false,
        local: false,
        requests: RequestExtras::NONE,
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
        // `sven` is the hosted service, not a server here.
        assert_eq!(local, ["brain", "dbus", "lmstudio", "ollama", "vllm"]);
    }

    #[test]
    fn gateways_declare_what_they_add_to_a_request() {
        let openrouter = get_driver("openrouter").unwrap().requests;
        assert!(openrouter.headers.contains(&("X-Title", "sven")));
        let rewrite = openrouter
            .rewrite_options
            .expect("openrouter rewrites options");
        assert_eq!(
            rewrite(serde_json::json!({"auto_router_allowed_models": ["a/*"], "k": 1})),
            serde_json::json!({"plugins": [{"id": "auto-router", "allowed_models": ["a/*"]}], "k": 1}),
        );
        assert_eq!(
            rewrite(serde_json::json!({"plugins": [], "auto_router_allowed_models": ["a/*"]})),
            serde_json::json!({"plugins": []}),
            "a raw plugins key wins"
        );
        assert_eq!(
            get_driver("portkey").unwrap().requests.headers_from_options,
            [("portkey_virtual_key", "x-portkey-virtual-key")]
        );
        assert!(get_driver("groq").unwrap().requests.headers.is_empty());
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
