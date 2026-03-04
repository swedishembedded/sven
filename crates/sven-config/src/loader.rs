// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::{debug, warn};

use crate::Config;

/// Ordered list of config file locations searched from lowest to highest priority.
/// Later files override earlier ones.
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System-wide default
    paths.push(PathBuf::from("/etc/sven/config.yaml"));
    paths.push(PathBuf::from("/etc/sven/config.yml"));

    // 2. XDG / home
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/sven/config.yaml"));
        paths.push(home.join(".config/sven/config.yml"));
    }
    if let Some(cfg) = dirs::config_dir() {
        paths.push(cfg.join("sven/config.yaml"));
        paths.push(cfg.join("sven/config.yml"));
    }

    // 3. Workspace-local
    paths.push(PathBuf::from(".sven/config.yaml"));
    paths.push(PathBuf::from(".sven/config.yml"));
    paths.push(PathBuf::from(".sven.yaml"));
    paths.push(PathBuf::from(".sven.yml"));
    paths.push(PathBuf::from("sven.yaml"));
    paths.push(PathBuf::from("sven.yml"));

    paths
}

/// Load configuration by merging all discovered YAML files.
/// The `extra` argument may provide an explicit path (e.g. `--config` CLI flag).
pub fn load(extra: Option<&Path>) -> anyhow::Result<Config> {
    let mut merged = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());

    for path in config_search_paths() {
        if path.is_file() {
            debug!(path = %path.display(), "loading config layer");
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let text = expand_env_vars(&raw, &path.display().to_string());
            let layer: serde_yaml::Value = serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            merge_yaml(&mut merged, layer);
        }
    }

    if let Some(p) = extra {
        debug!(path = %p.display(), "loading explicit config");
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let text = expand_env_vars(&raw, &p.display().to_string());
        let layer: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge_yaml(&mut merged, layer);
    }

    // Track whether the merged config contains an explicit model section before
    // deserialisation so we can skip auto-detection for users who have
    // configured their model explicitly.
    let has_model_config = merged.get("model").is_some();

    // Warn about any YAML keys that are not recognised by the schema before
    // deserialisation consumes (and silently discards) them.
    validate_unknown_fields(&merged, "");

    // Deserialize the merged YAML value into Config, falling back to defaults
    // when the merged value is empty (no config files found).
    let mut config: Config = if matches!(merged, serde_yaml::Value::Mapping(ref m) if m.is_empty())
    {
        Config::default()
    } else {
        serde_yaml::from_value(merged).unwrap_or_default()
    };

    // When no model has been explicitly configured, auto-select the best
    // available provider based on the API keys present in the environment.
    // Priority: Anthropic > OpenAI (both are excellent; Anthropic is ranked
    // first because claude-sonnet-4-6 is the recommended default).
    if !has_model_config {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            config.model.provider = "anthropic".into();
            config.model.name = "claude-sonnet-4-6".into();
        } else if std::env::var("OPENAI_API_KEY").is_ok() {
            config.model.provider = "openai".into();
            config.model.name = "gpt-5.2".into();
        }
        // If neither key is available the defaults remain, and from_config()
        // will produce a clear error when the provider is actually invoked.
    }

    // If model.provider references a named provider, expand it into a full
    // ModelConfig by merging the provider entry settings with the per-model
    // overrides.  This is the main mechanism that makes the new
    // provider-first config structure work.
    resolve_named_model_provider(&mut config);

    validate_token_limits(&config.model, "model");
    for (alias, entry) in &config.providers {
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

    Ok(config)
}

/// Warn when `max_tokens` (total context) is less than the sum of the
/// optional `max_input_tokens` and `max_output_tokens` limits.
///
/// The constraint is:
///   `max_tokens >= max_input_tokens + max_output_tokens`
fn validate_token_limits(cfg: &crate::ModelConfig, path: &str) {
    validate_model_params_token_limits(
        cfg.max_tokens,
        cfg.max_input_tokens,
        cfg.max_output_tokens,
        path,
    );
}

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

/// If `config.model.provider` matches a key in `config.providers`, expand it
/// into a full [`ModelConfig`] by applying provider-level defaults and any
/// per-model overrides registered for `config.model.name`.
fn resolve_named_model_provider(config: &mut Config) {
    let provider_key = config.model.provider.clone();
    let model_name = config.model.name.clone();

    if let Some(entry) = config.providers.get(&provider_key) {
        debug!(
            provider = %provider_key,
            model = %model_name,
            driver = %entry.name,
            "expanding named provider config"
        );
        config.model = entry.to_model_config(&model_name);
    }
}

// ── Environment variable expansion ───────────────────────────────────────────

/// Expand `${VAR}` and `${VAR:-default}` placeholders in config file text.
///
/// Uses [`shellexpand`] so the full bash-style variable syntax is supported:
///
/// | Syntax              | Behaviour                                              |
/// |---------------------|--------------------------------------------------------|
/// | `${VAR}`            | Replaced with `$VAR`; empty string + WARN if not set  |
/// | `${VAR:-default}`   | Replaced with `$VAR`; falls back to `default` silently |
/// | `$$`                | Literal `$` (escape sequence)                          |
///
/// `source_desc` is a human-readable label used in warning messages (typically
/// the config file path).
fn expand_env_vars(text: &str, source_desc: &str) -> String {
    // First pass: expand all set variables and handle `${VAR:-default}` for
    // unset ones.  Unset variables *without* a default remain as `${VAR}`.
    let first: Cow<str> =
        shellexpand::env_with_context_no_errors(text, |name| -> Option<Cow<str>> {
            std::env::var(name).ok().map(Cow::Owned)
        });

    // Second pass: any `${VAR}` placeholders that survived the first pass are
    // unset variables with no default.  Warn and substitute an empty string so
    // the YAML remains valid.
    let second: Cow<str> =
        shellexpand::env_with_context_no_errors(&*first, |name| -> Option<Cow<str>> {
            warn!(
                var = name,
                source = source_desc,
                "config env var is not set; substituting empty string"
            );
            Some(Cow::Borrowed(""))
        });

    second.into_owned()
}

// ── Unknown-field validation ──────────────────────────────────────────────────

/// Known top-level keys in [`Config`].
const CONFIG_KEYS: &[&str] = &["model", "agent", "tools", "tui", "providers"];

/// Known keys in [`crate::ModelConfig`].
const MODEL_CONFIG_KEYS: &[&str] = &[
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
];

/// Known keys in [`crate::ProviderEntry`].
const PROVIDER_ENTRY_KEYS: &[&str] = &[
    "name",
    "base_url",
    "api_key_env",
    "api_key",
    "models",
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
];

/// Known keys in [`crate::ModelParams`].
const MODEL_PARAMS_KEYS: &[&str] = &[
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
];

/// Known keys in [`crate::AgentConfig`].
const AGENT_CONFIG_KEYS: &[&str] = &[
    "default_mode",
    "max_tool_rounds",
    "compaction_threshold",
    "compaction_keep_recent",
    "compaction_strategy",
    "tool_result_token_cap",
    "compaction_overhead_reserve",
    "system_prompt",
    "max_step_timeout_secs",
    "max_run_timeout_secs",
];

/// Known keys in [`crate::ToolsConfig`].
const TOOLS_CONFIG_KEYS: &[&str] = &[
    "auto_approve_patterns",
    "deny_patterns",
    "timeout_secs",
    "use_docker",
    "docker_image",
    "web",
    "memory",
    "lints",
    "gdb",
];

/// Known keys in [`crate::TuiConfig`].
const TUI_CONFIG_KEYS: &[&str] = &["theme", "code_line_numbers", "wrap_width", "ascii_borders"];

/// Known keys in [`crate::WebConfig`].
const WEB_CONFIG_KEYS: &[&str] = &["search", "fetch_max_chars"];

/// Known keys in [`crate::WebSearchConfig`].
const WEB_SEARCH_CONFIG_KEYS: &[&str] = &["api_key"];

/// Known keys in [`crate::MemoryConfig`].
const MEMORY_CONFIG_KEYS: &[&str] = &["memory_file"];

/// Known keys in [`crate::LintsConfig`].
const LINTS_CONFIG_KEYS: &[&str] = &["rust_command", "typescript_command", "python_command"];

/// Known keys in [`crate::GdbConfig`].
const GDB_CONFIG_KEYS: &[&str] = &[
    "gdb_path",
    "command_timeout_secs",
    "connect_timeout_secs",
    "server_startup_wait_ms",
];

/// Recursively walk `value` and emit a `warn!` for any mapping key that is
/// not listed in the expected set for that schema level.
///
/// `path` is the dot-separated JSON path used in the warning message
/// (e.g. `"model"`, `"providers.my_ollama"`).
fn validate_unknown_fields(value: &serde_yaml::Value, path: &str) {
    let serde_yaml::Value::Mapping(map) = value else {
        return;
    };

    let (known, label): (&[&str], &str) = if path.is_empty() {
        (CONFIG_KEYS, "config")
    } else if path == "model" {
        (MODEL_CONFIG_KEYS, "model")
    } else if path == "agent" {
        (AGENT_CONFIG_KEYS, "agent")
    } else if path == "tools" {
        (TOOLS_CONFIG_KEYS, "tools")
    } else if path == "tools.web" {
        (WEB_CONFIG_KEYS, "tools.web")
    } else if path == "tools.web.search" {
        (WEB_SEARCH_CONFIG_KEYS, "tools.web.search")
    } else if path == "tools.memory" {
        (MEMORY_CONFIG_KEYS, "tools.memory")
    } else if path == "tools.lints" {
        (LINTS_CONFIG_KEYS, "tools.lints")
    } else if path == "tools.gdb" {
        (GDB_CONFIG_KEYS, "tools.gdb")
    } else if path == "tui" {
        (TUI_CONFIG_KEYS, "tui")
    } else if path == "providers" {
        // The providers map has arbitrary provider names as keys — all are valid.
        // We descend into each named entry to validate its fields.
        for (key, val) in map {
            let key_str = match key {
                serde_yaml::Value::String(s) => s.as_str(),
                _ => continue,
            };
            let child_path = format!("providers.{key_str}");
            validate_unknown_fields(val, &child_path);
        }
        return;
    } else if path.starts_with("providers.") {
        let rest = &path["providers.".len()..];
        if rest.contains('.') {
            // providers.<name>.models.<model_name> — per-model params
            (MODEL_PARAMS_KEYS, "model params")
        } else {
            // providers.<name> — provider entry
            (PROVIDER_ENTRY_KEYS, "provider entry")
        }
    } else {
        // Unknown path — skip validation to avoid false positives.
        return;
    };

    for (key, val) in map {
        let key_str = match key {
            serde_yaml::Value::String(s) => s.as_str(),
            _ => continue,
        };
        if !known.contains(&key_str) {
            warn!(
                "Unrecognised config field `{}.{}` — check spelling or update sven",
                path, key_str
            );
        } else {
            // Recurse into known nested sections.
            let child_path = if path.is_empty() {
                key_str.to_string()
            } else {
                format!("{path}.{key_str}")
            };
            match (label, key_str) {
                ("config", "model")
                | ("config", "agent")
                | ("config", "tools")
                | ("config", "tui")
                | ("config", "providers") => validate_unknown_fields(val, &child_path),
                ("tools", "web") | ("tools", "memory") | ("tools", "lints") | ("tools", "gdb") => {
                    validate_unknown_fields(val, &child_path)
                }
                ("tools.web", "search") => validate_unknown_fields(val, &child_path),
                ("provider entry", "models") => {
                    // Each key is a model name; validate its params.
                    if let serde_yaml::Value::Mapping(models_map) = val {
                        for (model_key, model_val) in models_map {
                            let model_name = match model_key {
                                serde_yaml::Value::String(s) => s.as_str(),
                                _ => continue,
                            };
                            let model_path = format!("{child_path}.{model_name}");
                            validate_unknown_fields(model_val, &model_path);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Deep-merge `src` into `dst`; src wins on scalar conflicts.
fn merge_yaml(dst: &mut serde_yaml::Value, src: serde_yaml::Value) {
    match (dst, src) {
        (serde_yaml::Value::Mapping(d), serde_yaml::Value::Mapping(s)) => {
            for (k, v) in s {
                let entry = d
                    .entry(k)
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                merge_yaml(entry, v);
            }
        }
        (dst, src) => *dst = src,
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn val(s: &str) -> serde_yaml::Value {
        serde_yaml::from_str(s).unwrap()
    }

    #[test]
    fn merge_scalar_src_wins() {
        let mut dst = val("x: 1");
        let src = val("x: 2");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["x"].as_i64(), Some(2));
    }

    #[test]
    fn merge_preserves_keys_not_in_src() {
        let mut dst = val("a: 1\nb: 2");
        let src = val("b: 99");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["a"].as_i64(), Some(1));
        assert_eq!(dst["b"].as_i64(), Some(99));
    }

    #[test]
    fn merge_nested_tables() {
        let mut dst = val("model:\n  provider: openai\n  name: gpt-4o");
        let src = val("model:\n  name: gpt-4o-mini");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["model"]["provider"].as_str(), Some("openai"));
        assert_eq!(dst["model"]["name"].as_str(), Some("gpt-4o-mini"));
    }

    #[test]
    fn load_returns_error_when_explicit_path_missing() {
        let result = load(Some(Path::new("/tmp/sven_nonexistent_config_xyz.yaml")));
        assert!(result.is_err());
    }

    #[test]
    fn load_with_no_extra_path_returns_valid_config() {
        // The provider may be auto-detected from env-vars (ANTHROPIC_API_KEY
        // or OPENAI_API_KEY), so we only assert that the result is a non-empty
        // provider string rather than a fixed value.
        let cfg = load(None).unwrap();
        assert!(!cfg.model.provider.is_empty());
        assert!(!cfg.model.name.is_empty());
    }

    #[test]
    fn load_explicit_file_overrides_defaults() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "model:\n  provider: anthropic\n  name: test-model").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "test-model");
    }

    #[test]
    fn load_resolves_named_provider_to_model_config() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
providers:
  my_ollama:
    name: openai
    base_url: http://localhost:8000/v1
    models:
      my-model:
        max_tokens: 54272
        driver_options:
          parse_tool_calls: false
model:
  provider: my_ollama
  name: my-model
"#
        )
        .unwrap();
        let cfg = load(Some(f.path())).unwrap();
        // After resolution, provider must be the actual driver ("openai"), not "my_ollama".
        assert_eq!(cfg.model.provider, "openai");
        assert_eq!(cfg.model.name, "my-model");
        assert_eq!(
            cfg.model.base_url.as_deref(),
            Some("http://localhost:8000/v1")
        );
        assert_eq!(cfg.model.max_tokens, Some(54272));
    }

    #[test]
    fn load_does_not_resolve_when_provider_is_builtin() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "model:\n  provider: anthropic\n  name: claude-opus-4-5").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "claude-opus-4-5");
    }

    // ── expand_env_vars ───────────────────────────────────────────────────────

    #[test]
    fn expand_env_vars_substitutes_set_variable() {
        std::env::set_var("SVEN_TEST_EXPAND_VAR", "hello");
        let result = expand_env_vars("key: ${SVEN_TEST_EXPAND_VAR}", "test");
        assert_eq!(result, "key: hello");
        std::env::remove_var("SVEN_TEST_EXPAND_VAR");
    }

    #[test]
    fn expand_env_vars_uses_default_for_unset_variable() {
        std::env::remove_var("SVEN_TEST_MISSING_VAR");
        let result = expand_env_vars("key: ${SVEN_TEST_MISSING_VAR:-fallback}", "test");
        assert_eq!(result, "key: fallback");
    }

    #[test]
    fn expand_env_vars_replaces_unset_required_var_with_empty() {
        std::env::remove_var("SVEN_TEST_REQUIRED_VAR");
        let result = expand_env_vars("key: ${SVEN_TEST_REQUIRED_VAR}", "test");
        assert_eq!(result, "key: ");
    }

    #[test]
    fn expand_env_vars_leaves_plain_text_unchanged() {
        let text = "model:\n  provider: openai\n  name: gpt-4o\n";
        assert_eq!(expand_env_vars(text, "test"), text);
    }

    #[test]
    fn load_expands_env_vars_in_config_file() {
        use std::io::Write;
        std::env::set_var("SVEN_TEST_PROVIDER", "anthropic");
        std::env::set_var("SVEN_TEST_MODEL", "claude-opus-4-5");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            "model:\n  provider: ${{SVEN_TEST_PROVIDER}}\n  name: ${{SVEN_TEST_MODEL}}"
        )
        .unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "claude-opus-4-5");
        std::env::remove_var("SVEN_TEST_PROVIDER");
        std::env::remove_var("SVEN_TEST_MODEL");
    }

    #[test]
    fn validate_unknown_fields_warns_for_unknown_top_level_key() {
        // This test just verifies the function does not panic for an unknown key.
        let yaml = val("model:\n  provider: openai\n  name: gpt-4o\nunknown_key: value\n");
        // validate_unknown_fields should not panic; tracing output is suppressed
        // in tests so we just check it doesn't crash.
        validate_unknown_fields(&yaml, "");
    }

    #[test]
    fn validate_unknown_fields_warns_for_unknown_model_key() {
        let yaml = val("model:\n  provider: openai\n  name: gpt-4o\n  nonexistent_field: value\n");
        validate_unknown_fields(&yaml, "");
    }

    #[test]
    fn validate_unknown_fields_accepts_all_known_top_level_keys() {
        let yaml =
            val("model:\n  provider: openai\n  name: gpt-4o\nagent:\n  max_tool_rounds: 100\n");
        // Should not produce any warnings — just verifying no panic.
        validate_unknown_fields(&yaml, "");
    }
}
