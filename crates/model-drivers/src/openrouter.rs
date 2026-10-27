// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What only the OpenRouter provider needs: its `driver_options` rewritten into
//! the request shape the API expects, and the background refresh of its model
//! catalog cache.

#[cfg(not(target_arch = "wasm32"))]
use sven_model::catalog;

use crate::ModelConfig;

/// Rewrite the `auto_router_allowed_models` convenience key in OpenRouter's
/// `driver_options` into the nested `plugins` structure the API expects:
///
/// ```yaml
/// driver_options:
///   auto_router_allowed_models: ["anthropic/*", "openai/gpt-5.1"]
/// ```
/// becomes:
/// ```json
/// { "plugins": [{ "id": "auto-router", "allowed_models": [...] }] }
/// ```
///
/// A raw `plugins` key is passed through unchanged.
pub(crate) fn transform_openrouter_options(cfg: &ModelConfig) -> serde_json::Value {
    let mut opts = cfg.driver_options.clone();
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

/// Spawn a background tokio task to refresh the OpenRouter model catalog cache.
///
/// The task fetches `GET <base_url>/models`, parses the rich OpenRouter
/// metadata (context window, output token cap, input modalities), then calls
/// [`catalog::cache_update`] to update both the in-memory live cache and the
/// on-disk file.
///
/// Only spawned when:
/// 1. A tokio runtime is already running (safe to call `tokio::spawn`).
/// 2. [`catalog::is_cache_stale`] reports that the on-disk cache is absent or
///    older than 24 hours.
/// 3. An API key is available (needed to authenticate the request).
///
/// Errors in the background task are silently discarded - cache refresh is a
/// best-effort optimisation, not a critical path.
#[cfg(target_arch = "wasm32")]
pub(crate) fn maybe_spawn_openrouter_cache_refresh(_api_key: Option<String>, _base_url: String) {
    // A browser has no on-disk catalog to refresh.
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn maybe_spawn_openrouter_cache_refresh(api_key: Option<String>, base_url: String) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return; // No runtime active - skip (e.g. unit tests, sync callers).
    };
    let Some(key) = api_key else {
        return; // No key - cannot authenticate the request.
    };
    if !catalog::is_cache_stale("openrouter") {
        return; // Fresh disk cache - no need to refresh.
    }
    handle.spawn(async move {
        let models_url = format!("{}/models", base_url.trim_end_matches('/'));
        let client = crate::build_http_client();
        let mut req = client
            .get(&models_url)
            .bearer_auth(&key)
            .header("HTTP-Referer", "https://github.com/svenai/sven")
            .header("X-Title", "sven")
            .timeout(std::time::Duration::from_secs(30));
        // Also set the header as `req =` to allow chaining (reqwest returns
        // the builder by value).
        req = req.header("User-Agent", "sven-model/cache-refresh");

        let Ok(resp) = req.send().await else { return };
        if !resp.status().is_success() {
            return;
        }
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return;
        };

        // Use the YAML-only entries for the meta-model fallback list so we
        // don't create a circular dependency with the live cache.
        let yaml_or_entries: Vec<catalog::ModelCatalogEntry> = catalog::yaml_catalog()
            .iter()
            .filter(|e| e.provider == "openrouter")
            .cloned()
            .collect();

        let entries =
            crate::openai_compat::parse_models_response(&body, "openrouter", &yaml_or_entries);
        if !entries.is_empty() {
            tracing::debug!(
                "openrouter cache refresh: {} models fetched and cached",
                entries.len()
            );
            catalog::cache_update("openrouter", entries);
        }
    });
}
