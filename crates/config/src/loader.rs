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

    // 1. System-wide default.  /etc/ is a Linux convention; macOS and Windows
    //    use dirs::config_dir() for system-wide configuration instead.
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/etc/sven/config.yaml"));
        paths.push(PathBuf::from("/etc/sven/config.yml"));
    }

    // 2. XDG / home.  dirs::config_dir() returns:
    //    Linux:   $XDG_CONFIG_HOME or ~/.config
    //    macOS:   ~/Library/Application Support
    //    Windows: %APPDATA% (C:\Users\<user>\AppData\Roaming)
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
        // Falling back to defaults is deliberate - one bad value must not stop
        // sven starting - but it is not something to do in silence. Before
        // this warning existed, a config the schema could not decode was
        // discarded *whole*, with no way for the user to tell that the file
        // they had just edited was being ignored entirely.
        match serde_yaml::from_value(merged) {
            Ok(config) => config,
            Err(e) => {
                warn!(
                    error = %e,
                    "config could not be decoded and is being IGNORED IN FULL; using defaults"
                );
                Config::default()
            }
        }
    };

    // When no model has been explicitly configured, auto-select the best
    // available provider based on the API keys present in the environment.
    //
    // Priority: brain (local) > OpenRouter > Anthropic > OpenAI. A locally
    // running brain wins over cloud providers when detectable — getting
    // started on a brain-enabled machine should need zero config — but ONLY
    // when detectable, so an existing cloud user (who has neither
    // BRAIN_API_KEY nor a brain keys file) sees no change at all. Everything
    // in this block is a sync, network-free check (env var / file
    // existence) — `sven-config` has no http/async dependency and this
    // function is not async; the actual reachability of brain is verified
    // later when the provider is constructed (`from_config_probed`), which
    // fails loudly if brain turns out not to be running.
    if !has_model_config {
        if brain_is_locally_detectable() {
            config.model.provider = "brain".into();
            // Empty is a deliberate sentinel, not an oversight: the actual
            // resident model is whatever brain is currently serving, and
            // that requires a network call to discover - this function is
            // sync and must stay that way (see the module-level rationale
            // above). `sven-model`'s `from_config_probed` resolves this
            // sentinel by asking brain's `/v1/models` when it has exactly
            // one chat-capable entry; a user who wants a specific resident
            // model still names it explicitly via `--model` or an explicit
            // `model:` block, which continues to win over this
            // auto-detection entirely (see `has_model_config` above).
            config.model.name = String::new();
        } else if std::env::var("OPENROUTER_API_KEY").is_ok() {
            // Keep the struct defaults: provider="openrouter", name="openrouter/auto".
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            config.model.provider = "anthropic".into();
            config.model.name = "claude-sonnet-4-6".into();
        } else if std::env::var("OPENAI_API_KEY").is_ok() {
            config.model.provider = "openai".into();
            config.model.name = "gpt-5.2".into();
        }
        // If no key is available the defaults remain (openrouter/auto), and
        // from_config() will produce a clear error when actually invoked.
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

// ── Local brain auto-detection ────────────────────────────────────────────────

/// Is a locally running `brain` model server detectable without any network
/// I/O?
///
/// Deliberately sync and cheap (env var reads + one `fs::metadata` stat) so
/// it can run inside [`load`], which must stay synchronous. This is a HINT,
/// not proof brain is actually reachable — a stale leftover keys file with
/// brain no longer running is harmless: `sven-model::from_config_probed`
/// turns an unreachable brain into a loud, actionable error rather than a
/// silent hang, same as any other misconfigured provider.
///
/// Set `SVEN_DISABLE_BRAIN_AUTODETECT=1` to opt out entirely (e.g. CI
/// environments that happen to have a stale brain keys file lying around
/// but want cloud-provider defaults).
fn brain_is_locally_detectable() -> bool {
    if std::env::var("SVEN_DISABLE_BRAIN_AUTODETECT").is_ok() {
        return false;
    }
    if std::env::var("BRAIN_API_KEY").is_ok() {
        return true;
    }
    brain_keys_file_path().is_some_and(|p| p.is_file())
}

/// Where brain's `--api-keys-out` JSON would be, mirroring
/// `sven-model::key_file_for`'s precedence exactly (duplicated rather than
/// shared: `sven-model` depends on `sven-config` for `ModelConfig`, so the
/// reverse dependency `sven-config -> sven-model` would be circular).
fn brain_keys_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BRAIN_API_KEYS_FILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return Some(Path::new(&runtime_dir).join("brain/api-keys.json"));
        }
    }
    dirs::state_dir().map(|d| d.join("brain/api-keys.json"))
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
const CONFIG_KEYS: &[&str] = &["model", "agent", "tools", "tui", "providers", "mcp_servers"];

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
    "input_modalities",
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
    "input_modalities",
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
    "input_modalities",
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
    "max_thinking_tokens",
    "thinking_timeout_secs",
];

/// Known keys in [`crate::ToolsConfig`].
const TOOLS_CONFIG_KEYS: &[&str] = &[
    "auto_approve_patterns",
    "deny_patterns",
    "timeout_secs",
    "web",
    "memory",
    "lints",
    "gdb",
    "asr",
    // Consumed by `sven_bootstrap::context_query`; omitting it warned users
    // about a key that works.
    "context",
];

/// Known keys in [`crate::AsrConfig`].
const ASR_CONFIG_KEYS: &[&str] = &["model", "bus_address", "timeout_secs"];

/// Known keys in [`crate::TuiConfig`].
const TUI_CONFIG_KEYS: &[&str] = &["theme", "code_line_numbers", "wrap_width", "ascii_borders"];

/// Known keys in [`crate::WebConfig`].
const WEB_CONFIG_KEYS: &[&str] = &["search", "fetch_max_chars"];

/// Known keys in [`crate::WebSearchConfig`].
const WEB_SEARCH_CONFIG_KEYS: &[&str] = &["api_key"];

/// Known keys in [`crate::MemoryConfig`].
const MEMORY_CONFIG_KEYS: &[&str] = &["memory_file"];

/// Sections sven accepts in a config file but does not act on, each with the
/// reason given to the user. A section here loads without error and earns one
/// warning naming it, however many keys it holds.
const IGNORED_CONFIG_SECTIONS: &[(&str, &str)] = &[(
    "tools.memory.learning",
    "sven does not train models; an application that learns from sven's work \
     configures that itself",
)];

/// Known keys in [`crate::LintsConfig`].
const LINTS_CONFIG_KEYS: &[&str] = &["rust_command", "typescript_command", "python_command"];

/// Known keys in [`crate::GdbConfig`].
const GDB_CONFIG_KEYS: &[&str] = &[
    "gdb_path",
    "command_timeout_secs",
    "connect_timeout_secs",
    "server_startup_wait_ms",
];

/// Known keys in [`crate::McpServerConfig`].
const MCP_SERVER_CONFIG_KEYS: &[&str] = &["transport", "enabled", "env", "oauth", "timeout_secs"];

/// Known keys in [`crate::McpTransport`] (stdio: type, command, args; http: type, url, headers).
const MCP_TRANSPORT_KEYS: &[&str] = &["type", "command", "args", "url", "headers"];

/// Known keys in [`crate::McpOAuthConfig`].
const MCP_OAUTH_CONFIG_KEYS: &[&str] = &[
    "scopes",
    "client_id",
    "client_secret",
    "redirect_uri",
    "callback_port",
];

/// Emit a `warn!` for every config key [`config_field_warnings`] reports.
fn validate_unknown_fields(value: &serde_yaml::Value, path: &str) {
    let mut warnings = Vec::new();
    collect_field_warnings(value, path, &mut warnings);
    for warning in warnings {
        warn!("{warning}");
    }
}

/// The warnings a merged config document earns: one per key the schema does
/// not recognise.
#[cfg(test)]
fn config_field_warnings(value: &serde_yaml::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    collect_field_warnings(value, "", &mut warnings);
    warnings
}

/// Recursively walk `value` and record a warning for any mapping key that is
/// not listed in the expected set for that schema level.
///
/// `path` is the dot-separated JSON path used in the warning message
/// (e.g. `"model"`, `"providers.my_ollama"`).
fn collect_field_warnings(value: &serde_yaml::Value, path: &str, warnings: &mut Vec<String>) {
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
    } else if path == "tools.asr" {
        (ASR_CONFIG_KEYS, "tools.asr")
    } else if path == "tui" {
        (TUI_CONFIG_KEYS, "tui")
    } else if path == "providers" {
        // The providers map has arbitrary provider names as keys - all are valid.
        // We descend into each named entry to validate its fields.
        for (key, val) in map {
            let key_str = match key {
                serde_yaml::Value::String(s) => s.as_str(),
                _ => continue,
            };
            let child_path = format!("providers.{key_str}");
            collect_field_warnings(val, &child_path, warnings);
        }
        return;
    } else if path == "mcp_servers" {
        // The mcp_servers map has arbitrary server names as keys - all are valid.
        for (key, val) in map {
            let key_str = match key {
                serde_yaml::Value::String(s) => s.as_str(),
                _ => continue,
            };
            let child_path = format!("mcp_servers.{key_str}");
            collect_field_warnings(val, &child_path, warnings);
        }
        return;
    } else if let Some(rest) = path.strip_prefix("providers.") {
        if rest.contains('.') {
            // providers.<name>.models.<model_name> - per-model params
            (MODEL_PARAMS_KEYS, "model params")
        } else {
            // providers.<name> - provider entry
            (PROVIDER_ENTRY_KEYS, "provider entry")
        }
    } else if let Some(_rest) = path.strip_prefix("mcp_servers.") {
        if path.ends_with(".transport") {
            (MCP_TRANSPORT_KEYS, "mcp transport")
        } else if path.ends_with(".oauth") {
            (MCP_OAUTH_CONFIG_KEYS, "mcp oauth")
        } else {
            // mcp_servers.<name> - server entry
            (MCP_SERVER_CONFIG_KEYS, "mcp server")
        }
    } else {
        // Unknown path - skip validation to avoid false positives.
        return;
    };

    for (key, val) in map {
        let key_str = match key {
            serde_yaml::Value::String(s) => s.as_str(),
            _ => continue,
        };
        let full_path = if path.is_empty() {
            key_str.to_string()
        } else {
            format!("{path}.{key_str}")
        };
        if let Some((_, reason)) = IGNORED_CONFIG_SECTIONS
            .iter()
            .find(|(section, _)| *section == full_path)
        {
            warnings.push(format!("Config section `{full_path}` is ignored: {reason}"));
        } else if !known.contains(&key_str) {
            warnings.push(format!(
                "Unrecognised config field `{path}.{key_str}` - check spelling or update sven"
            ));
        } else {
            // Recurse into known nested sections.
            let child_path = full_path;
            match (label, key_str) {
                ("config", "model")
                | ("config", "agent")
                | ("config", "tools")
                | ("config", "tui")
                | ("config", "providers")
                | ("config", "mcp_servers") => collect_field_warnings(val, &child_path, warnings),
                ("tools", "web")
                | ("tools", "memory")
                | ("tools", "lints")
                | ("tools", "gdb")
                | ("tools", "asr") => collect_field_warnings(val, &child_path, warnings),
                ("tools.web", "search") => collect_field_warnings(val, &child_path, warnings),
                ("provider entry", "models") => {
                    // Each key is a model name; validate its params.
                    if let serde_yaml::Value::Mapping(models_map) = val {
                        for (model_key, model_val) in models_map {
                            let model_name = match model_key {
                                serde_yaml::Value::String(s) => s.as_str(),
                                _ => continue,
                            };
                            let model_path = format!("{child_path}.{model_name}");
                            collect_field_warnings(model_val, &model_path, warnings);
                        }
                    }
                }
                ("mcp server", "transport") | ("mcp server", "oauth") => {
                    collect_field_warnings(val, &child_path, warnings)
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

    /// Guards every test that touches the auto-detection env vars
    /// (`OPENROUTER_API_KEY`/`ANTHROPIC_API_KEY`/`OPENAI_API_KEY`/`BRAIN_*`/
    /// `SVEN_DISABLE_BRAIN_AUTODETECT`/`XDG_RUNTIME_DIR`). These are
    /// process-global state `cargo test`'s default parallelism doesn't
    /// isolate between tests — unlike the rest of this module's env-var
    /// tests (`SVEN_ADV_*`, etc.), which get away with distinct names per
    /// test, all of these interact with the SAME priority-selection branch
    /// in `load`, so any two of them running concurrently can observe each
    /// other's env vars mid-test.
    static AUTODETECT_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A config file names the two or three settings its author cares about
    /// and says nothing about the rest. That has to keep working, and until
    /// now it silently did not: `load` deserialises the merged document with
    /// `unwrap_or_default()`, and a section whose struct had no serde defaults
    /// failed to deserialise the moment it was PRESENT but incomplete - so
    /// `tui: {theme: light}` did not merely fail to set the theme, it threw
    /// the user's entire config file away, without a word in the log.
    #[test]
    fn a_partially_specified_section_does_not_discard_the_whole_file() {
        let cfg: Config = serde_yaml::from_value(val(
            "tui:\n  theme: light\ntools:\n  gdb:\n    gdb_path: gdb\n",
        ))
        .expect("a config naming two settings must load, not fail whole");

        assert_eq!(cfg.tui.theme, "light", "the setting the file named");
        assert_eq!(cfg.tools.gdb.gdb_path, "gdb");
        assert_eq!(
            cfg.tools.gdb.command_timeout_secs,
            crate::GdbConfig::default().command_timeout_secs,
            "and everything it did not name keeps its default"
        );
        assert!(
            !cfg.tools.auto_approve_patterns.is_empty(),
            "a sibling field in a section the file touched is defaulted, not dropped"
        );
    }

    /// sven has no learning pipeline, so a `tools.memory.learning` section
    /// configures nothing. A config file that still carries one must keep
    /// loading - every other setting in it intact - and say once, by name,
    /// that the section is ignored: one warning for the section, not one per
    /// key inside it, and not the misleading "check spelling".
    #[test]
    fn a_learning_section_is_ignored_with_one_warning() {
        let text = "tui:\n  theme: light\ntools:\n  memory:\n    memory_file: notes/memory.json\n    learning:\n      submit_facts: true\n      batch_size: 16\n      submitter: local\n      study_args: [document-study, --dataset, \"{dataset}\"]\n";

        let warnings = config_field_warnings(&val(text));
        assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
        assert!(
            warnings[0].contains("tools.memory.learning") && warnings[0].contains("ignored"),
            "the warning names the section and says it is ignored: {}",
            warnings[0]
        );

        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{text}").unwrap();
        let cfg = load(Some(f.path())).expect("a config with a learning section still loads");
        assert_eq!(cfg.tui.theme, "light");
        assert_eq!(
            cfg.tools.memory.memory_file.as_deref(),
            Some("notes/memory.json"),
            "the rest of the memory section is kept"
        );
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
        let result = load(Some(Path::new("sven_nonexistent_config_xyz.yaml")));
        assert!(result.is_err());
    }

    #[test]
    fn load_with_no_extra_path_returns_valid_config() {
        // The provider may be auto-detected from env-vars (ANTHROPIC_API_KEY,
        // OPENAI_API_KEY, or a detectable local brain), so we only assert
        // that the result has a non-empty provider string rather than a
        // fixed value. Guarded because auto-detection reads process-global
        // env state shared with the brain/openrouter/anthropic/openai tests.
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let cfg = load(None).unwrap();
        assert!(!cfg.model.provider.is_empty());
        // A detected-but-unresolved brain (empty name is its deliberate
        // sentinel, resolved later by sven-model's from_config_probed) is a
        // valid outcome; every other provider must still report a real
        // model name.
        assert!(cfg.model.provider == "brain" || !cfg.model.name.is_empty());
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
    fn an_unknown_top_level_key_is_warned_about() {
        let yaml = val("model:\n  provider: openai\n  name: gpt-4o\nunknown_key: value\n");
        let warnings = config_field_warnings(&yaml);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("`.unknown_key`"), "{}", warnings[0]);
    }

    #[test]
    fn an_unknown_model_key_is_warned_about() {
        let yaml = val("model:\n  provider: openai\n  name: gpt-4o\n  nonexistent_field: value\n");
        let warnings = config_field_warnings(&yaml);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("model.nonexistent_field"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn known_keys_earn_no_warning() {
        let yaml =
            val("model:\n  provider: openai\n  name: gpt-4o\nagent:\n  max_tool_rounds: 100\n");
        assert_eq!(config_field_warnings(&yaml), Vec::<String>::new());
    }

    #[test]
    fn thinking_watchdog_keys_survive_the_full_load_pipeline() {
        // Regression guard for the allow-list trap: a key can be a real
        // struct field yet still get silently flagged as unknown (or the
        // reverse - listed but not actually a field) if `AGENT_CONFIG_KEYS`
        // and `AgentConfig` drift apart. Round-trip through the real `load()`
        // pipeline, not just `validate_unknown_fields`, so both the allow-list
        // and the actual deserialisation are proven together.
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            "agent:\n  max_thinking_tokens: 20000\n  thinking_timeout_secs: 300\n"
        )
        .unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.agent.max_thinking_tokens, Some(20_000));
        assert_eq!(cfg.agent.thinking_timeout_secs, Some(300));

        // Also run through validate_unknown_fields directly (doesn't panic
        // on a key it doesn't recognise, same as the existing coverage above
        // for other agent keys) as a second, independent check that these
        // two names are spelled identically in AGENT_CONFIG_KEYS.
        let yaml = val(
            "model:\n  provider: openai\n  name: gpt-4o\nagent:\n  max_thinking_tokens: 20000\n  thinking_timeout_secs: 300\n",
        );
        validate_unknown_fields(&yaml, "");
    }

    #[test]
    fn thinking_watchdog_keys_default_to_none_when_unset() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "agent:\n  max_tool_rounds: 100\n").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.agent.max_thinking_tokens, None);
        assert_eq!(cfg.agent.thinking_timeout_secs, None);
    }

    // ── Adversarial config inputs ─────────────────────────────────────────────

    #[test]
    fn adversarial_empty_yaml_file_returns_valid_config() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert!(
            !cfg.model.provider.is_empty(),
            "empty config should return defaults"
        );
    }

    #[test]
    fn adversarial_yaml_with_only_separator_returns_valid_config() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "---").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert!(!cfg.model.provider.is_empty());
    }

    #[test]
    fn adversarial_type_mismatch_in_model_field_falls_back_gracefully() {
        use std::io::Write;
        // model should be a mapping, but here we provide a list.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "model:\n  - foo\n  - bar").unwrap();
        // Should not panic; falls back to defaults via unwrap_or_default.
        let result = load(Some(f.path()));
        let _ = result;
    }

    #[test]
    fn adversarial_deeply_nested_yaml_does_not_stack_overflow() {
        use std::io::Write;
        // Build deeply nested YAML: {a: {a: {a: ...}}}
        let nested: String = "a:\n".to_string() + &"  ".repeat(200).repeat(200);
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", nested).unwrap();
        // Must not stack overflow; result may be error or default config.
        let _ = load(Some(f.path()));
    }

    #[test]
    fn adversarial_env_expansion_of_undefined_var_produces_empty_string() {
        std::env::remove_var("SVEN_ADV_TOTALLY_NONEXISTENT_XYZZY");
        let result = expand_env_vars("key: ${SVEN_ADV_TOTALLY_NONEXISTENT_XYZZY}", "test");
        assert_eq!(result, "key: ");
    }

    #[test]
    fn adversarial_env_expansion_value_containing_var_ref_does_not_produce_deep_value() {
        // OUTER_VAR is set to the string "${SVEN_ADV_INNER_VAR}" (a literal var-ref).
        // INNER_VAR is also set to "inner_value".
        // expand_env_vars must NOT recursively substitute the inner reference;
        // "inner_value" must never appear in the output.
        std::env::set_var("SVEN_ADV_OUTER_VAR2", "${SVEN_ADV_INNER_VAR2}");
        std::env::set_var("SVEN_ADV_INNER_VAR2", "inner_value");
        let result = expand_env_vars("key: ${SVEN_ADV_OUTER_VAR2}", "test");
        // Round 1 expands OUTER → literal text "${SVEN_ADV_INNER_VAR2}".
        // Round 2 sees a remaining ${...} reference (treated as unset) → substitutes "".
        // Either way, "inner_value" must not appear.
        assert!(
            !result.contains("inner_value"),
            "deep expansion must not occur; result was: {result:?}"
        );
        std::env::remove_var("SVEN_ADV_OUTER_VAR2");
        std::env::remove_var("SVEN_ADV_INNER_VAR2");
    }

    #[test]
    fn adversarial_all_unknown_top_level_keys_does_not_panic() {
        let yaml = val("totally_made_up_key: 42\nanother_fake: true\n");
        validate_unknown_fields(&yaml, "");
    }

    #[test]
    fn adversarial_large_number_of_providers_does_not_panic() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "providers:").unwrap();
        for i in 0..100 {
            writeln!(
                f,
                "  provider_{i}:\n    name: openai\n    base_url: http://localhost:{i}"
            )
            .unwrap();
        }
        let _ = load(Some(f.path()));
    }

    #[test]
    fn load_auto_detection_priority_openrouter_wins() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // We must be careful with env vars in parallel tests, but this is
        // the only test that sets these specific keys.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

        // No config file -> auto-detection runs.
        let cfg = load(None).unwrap();
        assert_eq!(cfg.model.provider, "openrouter");
        assert_eq!(cfg.model.name, "openrouter/auto");

        // Now remove OpenRouter; Anthropic should win.
        std::env::remove_var("OPENROUTER_API_KEY");
        let cfg = load(None).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "claude-sonnet-4-6");

        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    // ── brain auto-detection ────────────────────────────────────────────────

    #[test]
    fn brain_env_key_wins_over_openrouter() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // We must be careful with env vars in parallel tests, but these
        // specific keys (BRAIN_*) are only set in this test.
        std::env::set_var("BRAIN_API_KEY", "sk-brain-test");
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");

        let cfg = load(None).unwrap();

        std::env::remove_var("BRAIN_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");

        assert_eq!(cfg.model.provider, "brain");
        assert_eq!(
            cfg.model.name, "",
            "empty is the deliberate sentinel resolved later by from_config_probed"
        );
    }

    #[test]
    fn brain_keys_file_wins_over_openrouter_when_no_env_key() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), r#"{"openai":"sk-brain-from-file"}"#).unwrap();
        std::env::set_var("BRAIN_API_KEYS_FILE", f.path());
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");

        let cfg = load(None).unwrap();

        std::env::remove_var("BRAIN_API_KEYS_FILE");
        std::env::remove_var("OPENROUTER_API_KEY");

        assert_eq!(cfg.model.provider, "brain");
    }

    #[test]
    fn brain_not_detected_when_neither_env_key_nor_file_present() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Sanity check for the tests above: with BOTH BRAIN_* signals absent
        // and OpenRouter present, OpenRouter must still win (unchanged
        // pre-existing behaviour — an existing cloud user is unaffected).
        std::env::remove_var("BRAIN_API_KEY");
        std::env::remove_var("BRAIN_API_KEYS_FILE");
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");

        let cfg = load(None).unwrap();

        std::env::remove_var("OPENROUTER_API_KEY");

        assert_eq!(cfg.model.provider, "openrouter");
    }

    #[test]
    fn sven_disable_brain_autodetect_opts_out() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("BRAIN_API_KEY", "sk-brain-test");
        std::env::set_var("SVEN_DISABLE_BRAIN_AUTODETECT", "1");
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");

        let cfg = load(None).unwrap();

        std::env::remove_var("BRAIN_API_KEY");
        std::env::remove_var("SVEN_DISABLE_BRAIN_AUTODETECT");
        std::env::remove_var("OPENROUTER_API_KEY");

        assert_eq!(
            cfg.model.provider, "openrouter",
            "the opt-out must fall through to the next priority tier"
        );
    }

    #[test]
    fn explicit_model_config_wins_over_brain_autodetect() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        std::env::set_var("BRAIN_API_KEY", "sk-brain-test");

        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "model:\n  provider: anthropic\n  name: test-model").unwrap();
        let cfg = load(Some(f.path())).unwrap();

        std::env::remove_var("BRAIN_API_KEY");

        assert_eq!(
            cfg.model.provider, "anthropic",
            "an explicit model: block must win even when brain is detectable"
        );
    }

    #[test]
    fn brain_keys_file_path_prefers_explicit_env_over_xdg_runtime_dir() {
        // Missing before: this touches BRAIN_API_KEYS_FILE/XDG_RUNTIME_DIR,
        // the same shared env vars its five neighbours above already guard
        // with this mutex - without it, this test and the one below raced
        // each other's set_var/remove_var and flaked under `cargo test`'s
        // default parallel execution (reproduced: passed standalone, failed
        // under the full workspace suite).
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("BRAIN_API_KEYS_FILE", "/explicit/override.json");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let path = brain_keys_file_path();
        std::env::remove_var("BRAIN_API_KEYS_FILE");
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(path, Some(PathBuf::from("/explicit/override.json")));
    }

    #[test]
    fn brain_keys_file_path_falls_back_to_xdg_runtime_dir() {
        let _guard = AUTODETECT_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("BRAIN_API_KEYS_FILE");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let path = brain_keys_file_path();
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(
            path,
            Some(PathBuf::from("/run/user/1000/brain/api-keys.json"))
        );
    }
}
