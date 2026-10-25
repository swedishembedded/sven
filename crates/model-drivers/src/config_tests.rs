// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use super::*;

/// What the machine offers in a detection test.
#[derive(Default)]
struct Machine<'a> {
    /// Providers with a stored key (`sven auth set`).
    stored: &'a [&'a str],
    /// Whether a brain server's connection record is published.
    brain: bool,
    /// Variables set in the environment, which detection must not read.
    environment: &'a [(&'a str, &'a str)],
}

/// The model `settle` chooses with no `model:` section on `machine` alone.
fn detected_on(machine: &Machine) -> ModelConfig {
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::TEST_ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = std::env::temp_dir().join(format!("sven-detect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (config, runtime) = (root.join("config"), root.join("run"));
    std::fs::create_dir_all(config.join("sven")).unwrap();
    std::fs::create_dir_all(runtime.join("brain")).unwrap();
    std::fs::set_permissions(
        runtime.join("brain"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let keys: String = machine
        .stored
        .iter()
        .map(|p| format!("{p}: sk-{p}\n"))
        .collect();
    std::fs::write(config.join("sven/credentials.yaml"), keys).unwrap();
    if machine.brain {
        let record = runtime.join(format!("brain/serve-{}.json", std::process::id()));
        let body = serde_json::json!({"pid": std::process::id(), "surfaces": [
            {"provider": "openai", "url": "http://127.0.0.1:8788", "key": "sk-brain-record"}]});
        std::fs::write(&record, body.to_string()).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut vars: Vec<(&str, std::ffi::OsString)> = vec![
        ("XDG_CONFIG_HOME", config.into_os_string()),
        ("XDG_RUNTIME_DIR", runtime.into_os_string()),
    ];
    vars.extend(
        machine
            .environment
            .iter()
            .map(|(k, v)| (*k, std::ffi::OsString::from(v))),
    );
    let saved: Vec<_> = vars
        .iter()
        .map(|(k, _)| (*k, std::env::var_os(k)))
        .collect();
    for (key, value) in &vars {
        std::env::set_var(key, value);
    }
    let mut model = ModelConfig::default();
    settle(&mut model, &Providers::new(), false);
    for (key, value) in saved {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    model
}

// ── Defaults ─────────────────────────────────────────────────────────────

#[test]
fn the_default_model_is_openrouter_auto_with_the_registry_key() {
    let m = ModelConfig::default();
    assert_eq!(
        (m.provider.as_str(), m.name.as_str()),
        ("openrouter", "openrouter/auto")
    );
    // api_key_file must be None so resolve_api_key() falls through to the
    // driver registry; a hard-coded value would shadow it and send the wrong
    // key on a per-step provider override.
    assert!(m.api_key_file.is_none());
    assert!(m.api_key.is_none());
}

#[test]
fn caching_is_on_by_default_except_the_extended_ttl() {
    // The 1-hour TTL has a 2x write cost and is only worthwhile when turns
    // are more than 5 minutes apart.
    let m = ModelConfig::default();
    assert!(m.cache_system_prompt && m.cache_tools && m.cache_conversation);
    assert!(m.cache_images && m.cache_tool_results);
    assert!(!m.extended_cache_time);
}

#[test]
fn cache_flags_absent_from_yaml_keep_their_defaults_and_can_be_turned_off() {
    let m: ModelConfig = serde_yaml::from_str("provider: anthropic\nname: x\n").unwrap();
    assert!(m.cache_system_prompt && m.cache_tools && m.cache_conversation);
    assert!(m.cache_images && m.cache_tool_results && !m.extended_cache_time);

    let m: ModelConfig = serde_yaml::from_str(
        "cache_system_prompt: false\ncache_tools: false\ncache_conversation: false\n\
         cache_images: false\ncache_tool_results: false\nextended_cache_time: true\n",
    )
    .unwrap();
    assert!(!m.cache_system_prompt && !m.cache_tools && !m.cache_conversation);
    assert!(!m.cache_images && !m.cache_tool_results && m.extended_cache_time);
}

#[test]
fn providers_round_trip_through_yaml() {
    let yaml = "local:\n  name: openai\n  base_url: http://127.0.0.1:8080/v1\n  models:\n    phi-3:\n      max_tokens: 2048\n";
    let providers: Providers = serde_yaml::from_str(yaml).unwrap();
    let back: Providers =
        serde_yaml::from_str(&serde_yaml::to_string(&providers).unwrap()).unwrap();
    let p = back.get("local").unwrap();
    assert_eq!(p.name, "openai");
    assert_eq!(p.base_url.as_deref(), Some("http://127.0.0.1:8080/v1"));
    assert_eq!(p.models["phi-3"].max_tokens, Some(2048));
}

// ── Named providers ──────────────────────────────────────────────────────

#[test]
fn provider_entry_to_model_config_applies_provider_defaults() {
    let mut entry = ProviderEntry {
        name: "openai".into(),
        base_url: Some("http://local:8000/v1".into()),
        max_tokens: Some(8192),
        ..ProviderEntry::default()
    };
    entry.models.insert(
        "my-model".into(),
        ModelParams {
            max_tokens: Some(4096),
            ..ModelParams::default()
        },
    );
    let cfg = entry.to_model_config("my-model");
    assert_eq!(cfg.provider, "openai");
    assert_eq!(cfg.name, "my-model");
    assert_eq!(cfg.base_url.as_deref(), Some("http://local:8000/v1"));
    // per-model max_tokens overrides provider-level
    assert_eq!(cfg.max_tokens, Some(4096));
    // a model the entry does not list keeps the provider-level value
    assert_eq!(
        entry.to_model_config("unknown-model").max_tokens,
        Some(8192)
    );
}

#[test]
fn provider_entry_to_model_config_driver_options_override() {
    let mut entry = ProviderEntry::default();
    let driver_opts = serde_json::json!({"parse_tool_calls": false});
    entry.models.insert(
        "local-model".into(),
        ModelParams {
            driver_options: driver_opts.clone(),
            ..ModelParams::default()
        },
    );
    assert_eq!(
        entry.to_model_config("local-model").driver_options,
        driver_opts
    );
}

#[test]
fn input_modalities_are_inherited_and_overridden_per_model() {
    let mut entry = ProviderEntry {
        input_modalities: Some(vec!["text".into(), "image".into()]),
        ..ProviderEntry::default()
    };
    assert_eq!(
        entry.to_model_config("any").input_modalities.as_deref(),
        Some(&["text".to_string(), "image".to_string()][..])
    );
    entry.models.insert(
        "brain/omni".into(),
        ModelParams {
            input_modalities: Some(vec!["text".into(), "audio".into()]),
            ..ModelParams::default()
        },
    );
    assert_eq!(
        entry
            .to_model_config("brain/omni")
            .input_modalities
            .as_deref(),
        Some(&["text".to_string(), "audio".to_string()][..])
    );
    assert!(ProviderEntry::default()
        .to_model_config("gpt-4o")
        .input_modalities
        .is_none());
}

#[test]
fn a_model_naming_a_provider_entry_is_expanded_into_it() {
    let providers: Providers = serde_yaml::from_str(
        "my_ollama:\n  name: openai\n  base_url: http://localhost:8000/v1\n  models:\n    my-model:\n      max_tokens: 54272\n",
    )
    .unwrap();
    let mut model = ModelConfig {
        provider: "my_ollama".into(),
        name: "my-model".into(),
        ..ModelConfig::default()
    };
    settle(&mut model, &providers, true);
    // After expansion the provider is the driver, not the entry's name.
    assert_eq!(model.provider, "openai");
    assert_eq!(model.name, "my-model");
    assert_eq!(model.base_url.as_deref(), Some("http://localhost:8000/v1"));
    assert_eq!(model.max_tokens, Some(54272));
}

#[test]
fn a_builtin_provider_is_left_as_configured() {
    let mut model = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-opus-4-5".into(),
        ..ModelConfig::default()
    };
    settle(&mut model, &Providers::new(), true);
    assert_eq!(
        (model.provider.as_str(), model.name.as_str()),
        ("anthropic", "claude-opus-4-5")
    );
}

#[test]
fn model_reference_names_the_provider_alias_the_model_came_from() {
    let mut providers = Providers::new();
    providers.insert(
        "gateway".into(),
        ProviderEntry {
            name: "openai".into(),
            base_url: Some("http://gateway:4000/v1".into()),
            api_key_file: Some("GATEWAY_KEY".into()),
            ..ProviderEntry::default()
        },
    );
    let model = providers["gateway"].to_model_config("main");
    assert_eq!(model_reference(&model, &providers), "gateway/main");

    // Same driver, different endpoint: not the alias's config.
    let other = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o".into(),
        ..ModelConfig::default()
    };
    assert_eq!(model_reference(&other, &providers), "openai/gpt-4o");
}

// ── Detection ────────────────────────────────────────────────────────────

#[test]
fn an_explicit_model_section_is_never_replaced_by_detection() {
    let _guard = crate::TEST_ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut model = ModelConfig {
        provider: "anthropic".into(),
        name: "test-model".into(),
        ..ModelConfig::default()
    };
    settle(&mut model, &Providers::new(), true);
    assert_eq!(model.provider, "anthropic");
}

#[test]
fn openrouter_then_anthropic_then_openai_are_detected_in_that_order() {
    let m = detected_on(&Machine {
        stored: &["openrouter", "anthropic"],
        ..Machine::default()
    });
    assert_eq!(
        (m.provider.as_str(), m.name.as_str()),
        ("openrouter", "openrouter/auto")
    );
    let m = detected_on(&Machine {
        stored: &["anthropic", "openai"],
        ..Machine::default()
    });
    assert_eq!(
        (m.provider.as_str(), m.name.as_str()),
        ("anthropic", "claude-sonnet-4-6")
    );
    let m = detected_on(&Machine {
        stored: &["openai"],
        ..Machine::default()
    });
    assert_eq!(
        (m.provider.as_str(), m.name.as_str()),
        ("openai", "gpt-5.2")
    );
    let m = detected_on(&Machine::default());
    assert_eq!(m.provider, "openrouter", "no key leaves the defaults");
}

/// A key in the environment chooses nothing: keys are stored, not exported.
#[test]
fn a_key_in_the_environment_chooses_no_provider() {
    let m = detected_on(&Machine {
        environment: &[("ANTHROPIC_API_KEY", "sk-ant")],
        ..Machine::default()
    });
    assert_eq!(
        (m.provider.as_str(), m.name.as_str()),
        ("openrouter", "openrouter/auto")
    );
}

#[test]
fn a_running_brain_wins_over_stored_keys() {
    let m = detected_on(&Machine {
        stored: &["openrouter"],
        brain: true,
        ..Machine::default()
    });
    assert_eq!(m.provider, "brain");
    assert_eq!(
        m.name, "",
        "empty is the deliberate sentinel resolved later by from_config_probed"
    );
}

// ── Schema ───────────────────────────────────────────────────────────────

/// Every field of a section is a key its schema recognises: a key added to
/// the type but not to the schema would be warned about as unknown, and the
/// reverse would let a typo through.
#[test]
fn each_schema_names_exactly_its_types_fields() {
    fn keys_of(value: serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }
    fn reported(schema: &Schema, keys: &[String]) -> Vec<String> {
        let yaml: String = keys.iter().map(|k| format!("{k}: 1\n")).collect();
        sven_config::ConfigDocument::from_yaml(&yaml)
            .unwrap()
            .unknown_keys(schema)
    }
    let full_model = ModelConfig {
        api_key_file: Some(String::new()),
        input_modalities: Some(vec![]),
        ..ModelConfig::default()
    };
    let model_keys = keys_of(serde_json::to_value(full_model).unwrap());
    assert!(reported(&ModelConfig::schema(), &model_keys).is_empty());
    assert_eq!(model_keys.len(), 22);

    let full_params: ModelParams = serde_json::from_value(serde_json::json!({
        "max_tokens": 1, "max_output_tokens": 1, "max_input_tokens": 1, "temperature": 1.0,
        "driver_options": {}, "cache_system_prompt": true, "extended_cache_time": true,
        "cache_tools": true, "cache_conversation": true, "cache_images": true,
        "cache_tool_results": true, "mock_responses_file": "f", "input_modalities": []
    }))
    .unwrap();
    let params_keys = keys_of(serde_json::to_value(full_params).unwrap());
    assert!(reported(&ModelParams::schema(), &params_keys).is_empty());
    assert_eq!(params_keys.len(), 13);
}

#[test]
fn detected_brain_keeps_the_cloud_choice_in_reserve() {
    let m = detected_on(&Machine {
        stored: &["anthropic"],
        brain: true,
        ..Machine::default()
    });
    assert_eq!(m.provider, "brain");
    let reserve = m.detection_fallback.expect("a stored key is a fallback");
    assert_eq!(
        (reserve.provider.as_str(), reserve.name.as_str()),
        ("anthropic", "claude-sonnet-4-6")
    );
}

#[test]
fn detected_brain_without_a_stored_key_has_no_fallback() {
    let m = detected_on(&Machine {
        brain: true,
        ..Machine::default()
    });
    assert_eq!(m.provider, "brain");
    assert!(m.detection_fallback.is_none());
}
