// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Where a provider's API key comes from: explicit config, a named or
//! registry-default environment variable, or a provider's local keys file.

use crate::config::ModelConfig;
use sven_model::registry;

/// Path to a provider's local keys file, when it has one.
///
/// Only "brain" has this today: it mints a fresh random `sk-brain-<32hex>`
/// key on every start and can write it (plus every other surface's key) to
/// a JSON file via `--api-keys-out`. Reading that file each time a provider
/// is constructed (rather than caching) means a brain restart with a new
/// key just works on the next `sven` invocation, with no env var to
/// re-export by hand.
///
/// Precedence: `$BRAIN_API_KEYS_FILE` (explicit override) → per-user
/// `$XDG_RUNTIME_DIR/brain/api-keys.json` (tmpfs, appropriate for a value
/// that's regenerated every server start and shouldn't outlive a reboot) →
/// `~/.local/state/brain/api-keys.json` (falls back when no runtime dir is
/// set, e.g. some container/service setups).
pub(crate) fn key_file_for(provider: &str) -> Option<std::path::PathBuf> {
    if provider != "brain" {
        return None;
    }
    brain_keys_file()
}

/// Where brain writes its keys (`--api-keys-out`), by the precedence
/// [`key_file_for`] describes. Also what tells configuration loading that a
/// local brain is there to use.
pub(crate) fn brain_keys_file() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("BRAIN_API_KEYS_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return Some(std::path::Path::new(&runtime_dir).join("brain/api-keys.json"));
        }
    }
    dirs::state_dir().map(|d| d.join("brain/api-keys.json"))
}

/// Read a single dialect's key out of a brain-shaped keys JSON file:
/// `{"<dialect>": "<key>", ...}`. Pure and path-parameterised (no env
/// lookup) so it is directly unit-testable without touching process state.
pub(crate) fn read_key_from_json_file(path: &std::path::Path, dialect_key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get(dialect_key)?.as_str().map(str::to_string)
}

/// Read a provider's key out of its keys file (see [`key_file_for`]).
///
/// brain's `--api-keys-out` format is `{"<dialect>": "<key>", ...}` - one
/// entry per surface it serves (`"openai"`/`"anthropic"`/`"openrouter"`,
/// brain's `Provider::as_str()`), NOT `"brain"` - sven's driver id and
/// brain's dialect name are different namespaces that happen to both exist.
/// sven's "brain" driver always targets brain's OpenAI-compatible surface
/// (base_url ends in `/v1`, throughout this codebase), so the JSON lookup
/// is hardcoded to `"openai"` regardless of what `provider` (sven's id) is.
pub(crate) fn read_key_from_file(provider: &str) -> Option<String> {
    read_key_from_json_file(&key_file_for(provider)?, "openai")
}

pub(crate) fn resolve_api_key(cfg: &ModelConfig) -> Option<String> {
    if let Some(k) = &cfg.api_key {
        return Some(k.clone());
    }
    if let Some(env) = &cfg.api_key_env {
        return std::env::var(env).ok();
    }
    // Auto-resolve from registry default env var if neither is set.
    if let Some(meta) = registry::get_driver(&cfg.provider) {
        if let Some(env_var) = meta.default_api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                return Some(key);
            }
        }
    }
    // Last resort: a provider-specific keys file (see key_file_for).
    read_key_from_file(&cfg.provider)
}
