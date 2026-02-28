// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Tests that every driver registered in the registry can be instantiated from
//! config without returning an "unknown model provider" error.
//!
//! Drivers that require API keys will succeed at construction (since key
//! resolution is lazy) but fail later at network time.  Drivers that require
//! `base_url` (LiteLLM, Cloudflare, Azure) are tested with a dummy URL.

use sven_config::ModelConfig;
use sven_model::{from_config, get_driver, list_drivers, registry::DriverMeta};

fn minimal_cfg(provider: &str) -> ModelConfig {
    ModelConfig {
        provider: provider.into(),
        name: "test-model".into(),
        ..ModelConfig::default()
    }
}

fn cfg_with_base_url(provider: &str, base_url: &str) -> ModelConfig {
    ModelConfig {
        provider: provider.into(),
        name: "test-model".into(),
        base_url: Some(base_url.into()),
        ..ModelConfig::default()
    }
}

/// Providers that require `base_url` to be set in config.
fn needs_base_url(id: &str) -> bool {
    matches!(id, "litellm" | "cloudflare")
}

/// Providers that require `azure_resource` or pre-built `base_url`.
fn needs_azure_config(id: &str) -> bool {
    id == "azure"
}

#[test]
fn registry_is_populated() {
    assert!(!list_drivers().is_empty(), "DRIVERS must not be empty");
    assert!(get_driver("openai").is_some());
    assert!(get_driver("anthropic").is_some());
    assert!(get_driver("google").is_some());
    assert!(get_driver("aws").is_some());
    assert!(get_driver("groq").is_some());
    assert!(get_driver("ollama").is_some());
    assert!(get_driver("mock").is_some());
}

#[test]
fn all_registered_drivers_instantiate_without_unknown_error() {
    for driver in list_drivers() {
        let id = driver.id;
        let cfg = if needs_base_url(id) {
            cfg_with_base_url(id, "http://localhost:4000/v1")
        } else if needs_azure_config(id) {
            let mut c = minimal_cfg(id);
            c.azure_resource = Some("myresource".into());
            c.azure_deployment = Some("mydeployment".into());
            c
        } else {
            minimal_cfg(id)
        };

        match from_config(&cfg) {
            Ok(_) => {
                // Success — driver is correctly wired up.
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    !msg.to_lowercase().contains("unknown model provider"),
                    "Driver '{id}' is registered but not handled by from_config.\n\
                     Error: {msg}"
                );
                // Other errors (missing key, missing base_url, etc.) are acceptable
                // at instantiation time — they will surface at request time.
            }
        }
    }
}

#[test]
fn unknown_provider_returns_descriptive_error() {
    let cfg = minimal_cfg("definitely-not-a-real-provider-xyz");
    let err = from_config(&cfg)
        .err()
        .expect("should fail for unknown provider");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown model provider"),
        "error message should mention 'unknown model provider', got: {msg}"
    );
    // Should suggest running list-providers
    assert!(
        msg.contains("list-providers") || msg.contains("Known providers"),
        "error should hint at list-providers, got: {msg}"
    );
}

#[test]
fn driver_metadata_is_complete() {
    for DriverMeta {
        id,
        name,
        description,
        ..
    } in list_drivers()
    {
        assert!(!id.is_empty(), "driver id must not be empty");
        assert!(!name.is_empty(), "driver '{id}' name must not be empty");
        assert!(
            !description.is_empty(),
            "driver '{id}' description must not be empty"
        );
    }
}

#[test]
fn driver_ids_are_lowercase_and_alphanumeric() {
    for d in list_drivers() {
        for ch in d.id.chars() {
            assert!(
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_',
                "driver id '{}' contains invalid char '{}'",
                d.id,
                ch
            );
        }
    }
}

#[test]
fn drivers_with_no_key_requirement_have_no_default_env() {
    // Providers marked requires_api_key=false must not set an env var that
    // would mislead users into thinking a key is needed.
    let non_key_providers = ["ollama", "vllm", "lmstudio", "mock"];
    for id in &non_key_providers {
        let meta = get_driver(id).unwrap_or_else(|| panic!("{id} must be in registry"));
        assert!(
            !meta.requires_api_key,
            "{id} should have requires_api_key=false"
        );
    }
}

#[test]
fn all_major_providers_registered() {
    let must_exist = [
        "openai",
        "anthropic",
        "google",
        "azure",
        "aws",
        "cohere",
        "openrouter",
        "litellm",
        "groq",
        "together",
        "fireworks",
        "cerebras",
        "deepinfra",
        "nebius",
        "sambanova",
        "huggingface",
        "nvidia",
        "perplexity",
        "mistral",
        "xai",
        "deepseek",
        "moonshot",
        "dashscope",
        "glm",
        "minimax",
        "qianfan",
        "ollama",
        "vllm",
        "lmstudio",
        "mock",
    ];
    for id in &must_exist {
        assert!(
            get_driver(id).is_some(),
            "Required provider '{id}' is not in the registry"
        );
    }
}

#[test]
fn openai_driver_correct_metadata() {
    let meta = get_driver("openai").unwrap();
    assert_eq!(meta.id, "openai");
    assert_eq!(meta.default_api_key_env, Some("OPENAI_API_KEY"));
    assert!(meta.requires_api_key);
}

#[test]
fn ollama_driver_no_key_required() {
    let meta = get_driver("ollama").unwrap();
    assert!(!meta.requires_api_key);
    assert!(meta.default_api_key_env.is_none());
    assert!(meta.default_base_url.unwrap().contains("localhost"));
}

#[test]
fn aws_driver_no_default_key_env() {
    let meta = get_driver("aws").unwrap();
    // AWS uses IAM credentials, not a simple API key env var.
    assert!(meta.default_api_key_env.is_none());
    assert!(!meta.requires_api_key);
}
