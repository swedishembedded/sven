// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Context-window discovery for an OpenAI-compatible server: the two probes
//! `probe_context_window` tries, and the server root the first one needs.

use super::OpenAICompatProvider;

impl OpenAICompatProvider {
    /// Discovery path 1: llama.cpp-style `GET {server_root}/props`.
    /// See [`Self::probe_context_window`]'s doc comment for why this only
    /// makes sense for a single-model server, and short-timeouts since it is
    /// a startup hint, not a critical path.
    pub(super) async fn probe_context_window_via_props(&self) -> Option<u32> {
        let root = self.server_root.as_deref()?;
        let props_url = format!("{root}/props");
        let req = self.authed(
            self.client
                .get(&props_url)
                .timeout(std::time::Duration::from_secs(5)),
        );
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        // llama.cpp /props returns {"n_ctx": <u32>, ...}
        body["n_ctx"].as_u64().map(|v| v as u32)
    }

    /// Discovery path 2: find this provider's own model in the standard
    /// `GET {models_url}` list and read its `context_length`. Per-model, so
    /// this is correct for a server hosting several models with different
    /// capacities - unlike `/props`, which has no way to name a model.
    pub(super) async fn probe_context_window_via_models_list(&self) -> Option<u32> {
        let url = self.models_url.as_deref()?;
        let req = self.authed(
            self.client
                .get(url)
                .timeout(std::time::Duration::from_secs(5)),
        );
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        // Real OpenAI reports no context size on any entry, so hosted
        // OpenAI finds nothing here - identical to `None`.
        crate::discovery::parse_served_models(&body)?
            .into_iter()
            .find(|m| m.id == self.model)?
            .context_length
    }
}

/// Derive the server root from a `/v1`-prefixed API base URL.
///
/// Strips common API path suffixes so that `probe_context_window()` can reach
/// the server's properties endpoint (e.g. llama.cpp's `GET /props`).
///
/// Examples:
/// - `http://localhost:8080/v1`   → `http://localhost:8080`
/// - `https://api.openai.com/v1` → `https://api.openai.com`
/// - `http://host:8080/api/v1`   → `http://host:8080`
/// - `http://host:8080`          → `http://host:8080` (unchanged)
pub(super) fn derive_server_root(base_url: &str) -> String {
    let b = base_url.trim_end_matches('/');
    if let Some(root) = b.strip_suffix("/api/v1") {
        return root.to_string();
    }
    if let Some(root) = b.strip_suffix("/v1") {
        return root.to_string();
    }
    b.to_string()
}
