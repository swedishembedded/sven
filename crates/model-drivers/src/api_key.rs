// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Where a provider's API key comes from, in order: the configuration
//! (`api_key`, else the file `api_key_file` names), the key the user stored
//! for the provider (`sven auth set <provider>`), and for a local brain the
//! connection record of the server sven talks to. The environment is never
//! read for a key.
//!
//! A stored key is sent only to its provider's own endpoint. Configuration
//! is also read from the working directory, so a `base_url` may come from a
//! repository the user merely cloned; a key stored for OpenAI must never
//! follow it to someone else's server. Another endpoint names its own key.

use crate::config::ModelConfig;
use sven_model::registry;

/// The key `cfg` is used with, if it has one.
pub(crate) fn resolve_api_key(cfg: &ModelConfig) -> Option<String> {
    if let Some(k) = &cfg.api_key {
        return Some(k.clone());
    }
    if let Some(file) = &cfg.api_key_file {
        return read_key_file(file).ok();
    }
    if at_default_endpoint(cfg) {
        if let Some(k) = crate::credentials::get(&cfg.provider) {
            return Some(k);
        }
    }
    if cfg.provider == "brain" {
        return crate::brain_local::find(cfg.base_url.as_deref()).map(|b| b.key);
    }
    None
}

/// Whether `cfg` talks to its driver's own fixed endpoint: the registry's
/// default URL, given or implied. A driver with no default (Azure, LiteLLM,
/// Cloudflare) is always at an endpoint the configuration chose.
fn at_default_endpoint(cfg: &ModelConfig) -> bool {
    let Some(default) = registry::get_driver(&cfg.provider).and_then(|m| m.default_base_url) else {
        return false;
    };
    cfg.base_url
        .as_deref()
        .is_none_or(|url| default.trim_end_matches('/') == url.trim_end_matches('/'))
}

/// The one key a key file holds: its contents, whitespace trimmed. `~/` at
/// the start of the path is the home directory.
pub(crate) fn read_key_file(path: &str) -> anyhow::Result<String> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("{path}: no home directory"))?
            .join(rest),
        None => std::path::PathBuf::from(path),
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("api_key_file {}: {e}", path.display()))?;
    let key = text.trim();
    anyhow::ensure!(
        !key.is_empty(),
        "api_key_file {} holds no key",
        path.display()
    );
    Ok(key.to_string())
}
