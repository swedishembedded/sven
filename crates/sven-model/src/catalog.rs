// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Model catalog: static metadata (bundled YAML) with a live-cache overlay.
//!
//! # Lookup order
//!
//! Every public lookup function checks the **in-memory live cache** first.
//! The live cache is populated from two sources:
//!
//! 1. **Disk cache** — a per-provider JSON file in
//!    `~/.config/sven/model-cache/<provider>.json` written by previous
//!    sessions.  Loaded at most once per provider per process via
//!    [`load_disk_cache`].  Entries are only used when the file is younger
//!    than [`CACHE_TTL_SECS`].
//!
//! 2. **Background refresh** — a tokio task spawned by the model provider
//!    after a successful live `GET /models` call.  Updates both the in-memory
//!    cache and the on-disk file via [`cache_update`].
//!
//! The static YAML catalog (`models.yaml`) is always consulted as a fallback
//! so that offline use and uncommon providers degrade gracefully.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a disk-cached model list is considered fresh.
const CACHE_TTL_SECS: u64 = 86_400; // 24 hours

// ── Public types ──────────────────────────────────────────────────────────────

/// Input modalities supported by a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
}

fn default_input_modalities() -> Vec<InputModality> {
    // Conservative default: text only.
    // Vision-capable models must explicitly list `image` in models.yaml.
    vec![InputModality::Text]
}

/// Metadata for a single model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    /// Provider-scoped model identifier (e.g. "gpt-4o", "claude-opus-4-6")
    pub id: String,
    /// Human-readable display name
    pub name: String,
    /// Provider identifier: "openai" | "anthropic" | "mock"
    pub provider: String,
    /// Total context window in tokens (input + output)
    pub context_window: u32,
    /// Maximum output tokens per completion
    pub max_output_tokens: u32,
    /// Short description
    #[serde(default)]
    pub description: String,
    /// Supported input modalities.  Defaults to `[text]`.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
}

impl ModelCatalogEntry {
    /// Return `true` if the model can accept image input.
    pub fn supports_images(&self) -> bool {
        self.input_modalities.contains(&InputModality::Image)
    }
}

// ── Static YAML catalog ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CatalogFile {
    models: Vec<ModelCatalogEntry>,
}

/// Return a reference to the parsed static YAML catalog.
///
/// Parsed exactly once via [`OnceLock`]; subsequent calls are zero-cost
/// pointer returns.  This is the raw YAML-only view — callers that want
/// live-cache entries should use the public API functions.
pub(crate) fn yaml_catalog() -> &'static [ModelCatalogEntry] {
    static CATALOG: OnceLock<Vec<ModelCatalogEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let yaml = include_str!("../models.yaml");
        let file: CatalogFile =
            serde_yaml::from_str(yaml).expect("bundled models.yaml must be valid");
        file.models
    })
}

// ── In-memory live cache ──────────────────────────────────────────────────────

/// Global in-memory live cache keyed by provider id.
///
/// - Populated from disk on first access per provider ([`load_disk_cache`]).
/// - Updated by background refresh tasks ([`cache_update`]).
/// - Consulted by [`lookup`] / [`lookup_by_model_name`] before the YAML.
fn live_cache() -> &'static RwLock<HashMap<String, Vec<ModelCatalogEntry>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Vec<ModelCatalogEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Tracks which providers have already had their disk cache loaded this run.
fn disk_loaded() -> &'static Mutex<HashSet<String>> {
    static LOADED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOADED.get_or_init(|| Mutex::new(HashSet::new()))
}

// ── Disk cache helpers ────────────────────────────────────────────────────────

/// On-disk serialisation for a single provider's live model cache.
#[derive(Debug, Serialize, Deserialize)]
struct DiskCache {
    /// Unix timestamp (seconds) of when this cache was written.
    fetched_at: u64,
    entries: Vec<ModelCatalogEntry>,
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn disk_cache_path(provider: &str) -> Option<std::path::PathBuf> {
    let base = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|p| p.join("sven").join("model-cache"))?;
    Some(base.join(format!("{provider}.json")))
}

// ── Public cache API ──────────────────────────────────────────────────────────

/// Load the on-disk cache for `provider` into the in-memory live cache.
///
/// Called at most once per provider per process (guarded by [`disk_loaded`]).
/// Does nothing when the file is absent, unreadable, invalid JSON, or older
/// than [`CACHE_TTL_SECS`].
pub fn load_disk_cache(provider: &str) {
    // Guard: only load once per provider per process.
    {
        let mut set = disk_loaded().lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(provider.to_string()) {
            return;
        }
    }

    let path = match disk_cache_path(provider) {
        Some(p) => p,
        None => return,
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => return,
    };
    let dc: DiskCache = match serde_json::from_slice(&data) {
        Ok(v) => v,
        Err(_) => return,
    };
    if current_unix_secs().saturating_sub(dc.fetched_at) > CACHE_TTL_SECS {
        // Stale — skip; background refresh will repopulate.
        return;
    }
    if let Ok(mut guard) = live_cache().write() {
        guard.insert(provider.to_string(), dc.entries);
    }
}

/// Return `true` when the on-disk cache for `provider` is absent or expired.
///
/// Used by the background refresh logic to decide whether a new fetch is
/// needed.
pub fn is_cache_stale(provider: &str) -> bool {
    let path = match disk_cache_path(provider) {
        Some(p) => p,
        None => return true,
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => return true,
    };
    let dc: DiskCache = match serde_json::from_slice(&data) {
        Ok(v) => v,
        Err(_) => return true,
    };
    current_unix_secs().saturating_sub(dc.fetched_at) > CACHE_TTL_SECS
}

/// Update the in-memory live cache for `provider` and persist to disk.
///
/// Called by:
/// - [`OpenAICompatProvider::list_models`] after a successful live fetch.
/// - The background refresh task spawned from `from_config`.
pub fn cache_update(provider: &str, entries: Vec<ModelCatalogEntry>) {
    if let Ok(mut guard) = live_cache().write() {
        guard.insert(provider.to_string(), entries.clone());
    }
    // Best-effort disk write; errors are silently ignored.
    let _ = persist_to_disk(provider, &entries);
}

fn persist_to_disk(provider: &str, entries: &[ModelCatalogEntry]) -> anyhow::Result<()> {
    let path =
        disk_cache_path(provider).ok_or_else(|| anyhow::anyhow!("cannot determine config dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dc = DiskCache {
        fetched_at: current_unix_secs(),
        entries: entries.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&dc)?;
    std::fs::write(&path, &json)?;
    Ok(())
}

// ── Public lookup API (live-cache-first) ──────────────────────────────────────

/// Return all known models: live-cache entries (by provider) merged over the
/// static YAML catalog.
///
/// When a provider has an active live-cache entry, its static YAML entries are
/// replaced entirely by the live data.  Providers without a live-cache entry
/// keep their static entries unchanged.
pub fn static_catalog() -> Vec<ModelCatalogEntry> {
    let mut result = yaml_catalog().to_vec();

    if let Ok(guard) = live_cache().read() {
        if guard.is_empty() {
            return result;
        }
        // Replace YAML entries for providers that have live data.
        for (provider, live_entries) in guard.iter() {
            result.retain(|e| &e.provider != provider);
            result.extend(live_entries.iter().cloned());
        }
    }
    result
}

/// Look up a single model by provider and id (or name).
///
/// Checks the in-memory live cache first; falls back to the static YAML
/// catalog.  Returns `None` if not found in either source.
pub fn lookup(provider: &str, model_id: &str) -> Option<ModelCatalogEntry> {
    // Live cache first.
    if let Ok(guard) = live_cache().read() {
        if let Some(entries) = guard.get(provider) {
            if let Some(e) = entries
                .iter()
                .find(|e| e.id == model_id || e.name == model_id)
            {
                return Some(e.clone());
            }
        }
    }
    // Static YAML fallback.
    yaml_catalog()
        .iter()
        .find(|e| e.provider == provider && (e.id == model_id || e.name == model_id))
        .cloned()
}

/// Look up a model by bare model name (without a provider prefix).
///
/// Searches `id` and `name` fields.  Live cache is checked first (all
/// providers), then the static YAML catalog.  Returns the first match.
///
/// Used by `ModelResolver` step 3 to detect when a bare model name (e.g.
/// `"gpt-4o"`) can be resolved against a known provider from the catalog.
pub fn lookup_by_model_name(model_name: &str) -> Option<ModelCatalogEntry> {
    // Live cache first (all providers).
    if let Ok(guard) = live_cache().read() {
        for entries in guard.values() {
            if let Some(e) = entries
                .iter()
                .find(|e| e.id == model_name || e.name == model_name)
            {
                return Some(e.clone());
            }
        }
    }
    // Static YAML fallback.
    yaml_catalog()
        .iter()
        .find(|e| e.id == model_name || e.name == model_name)
        .cloned()
}

/// Return `true` if the model supports image input.
///
/// Defaults to `false` when the model is not found in either the live cache
/// or the static YAML catalog.
pub fn supports_images(provider: &str, model_id: &str) -> bool {
    lookup(provider, model_id)
        .map(|e| e.supports_images())
        .unwrap_or(false)
}

/// Look up the context window for a model.  Falls back to `default` if not found.
pub fn context_window(provider: &str, model_id: &str, default: u32) -> u32 {
    lookup(provider, model_id)
        .map(|e| e.context_window)
        .unwrap_or(default)
}

/// Look up the max output tokens for a model.  Falls back to `default` if not found.
pub fn max_output_tokens(provider: &str, model_id: &str, default: u32) -> u32 {
    lookup(provider, model_id)
        .map(|e| e.max_output_tokens)
        .unwrap_or(default)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_catalog_is_non_empty() {
        let models = static_catalog();
        assert!(
            !models.is_empty(),
            "bundled catalog must contain at least one model"
        );
    }

    #[test]
    fn gpt4o_is_in_catalog() {
        let entry = lookup("openai", "gpt-4o").expect("gpt-4o must be in catalog");
        assert_eq!(entry.provider, "openai");
        assert!(entry.context_window >= 128_000);
        assert!(entry.max_output_tokens >= 4_096);
    }

    #[test]
    fn gpt4o_supports_images() {
        let entry = lookup("openai", "gpt-4o").unwrap();
        assert!(entry.supports_images(), "gpt-4o must support image input");
    }

    #[test]
    fn claude_opus_is_in_catalog() {
        let entry =
            lookup("anthropic", "claude-opus-4-6").expect("claude-opus-4-6 must be in catalog");
        assert_eq!(entry.provider, "anthropic");
        assert!(entry.context_window >= 200_000);
    }

    #[test]
    fn claude_opus_supports_images() {
        let entry = lookup("anthropic", "claude-opus-4-6").unwrap();
        assert!(
            entry.supports_images(),
            "claude-opus-4-6 must support image input"
        );
    }

    #[test]
    fn lookup_unknown_model_returns_none() {
        assert!(lookup("openai", "nonexistent-model-xyz").is_none());
    }

    #[test]
    fn context_window_fallback_used_when_unknown() {
        let cw = context_window("openai", "no-such-model", 4096);
        assert_eq!(cw, 4096);
    }

    #[test]
    fn live_cache_takes_precedence_over_yaml() {
        // Inject a fake entry into the live cache and verify lookup returns it.
        let fake = ModelCatalogEntry {
            id: "live-test-model-xyz".to_string(),
            name: "Live Test Model".to_string(),
            provider: "openai".to_string(),
            context_window: 999_999,
            max_output_tokens: 88_888,
            description: "injected for test".to_string(),
            input_modalities: vec![InputModality::Text],
        };
        cache_update("openai", vec![fake.clone()]);
        let found = lookup("openai", "live-test-model-xyz").expect("should find live entry");
        assert_eq!(found.context_window, 999_999);
        // Clean up so other tests are not affected.
        if let Ok(mut guard) = live_cache().write() {
            guard.remove("openai");
        }
    }

    #[test]
    fn all_yaml_entries_have_non_zero_windows() {
        for entry in yaml_catalog() {
            // Non-completion models (video generation, etc.) may have zero windows.
            if entry.context_window == 0 || entry.max_output_tokens == 0 {
                // Sanity: such entries should describe themselves as non-token models.
                assert!(
                    entry.description.to_lowercase().contains("video")
                        || entry.description.to_lowercase().contains("non-token")
                        || entry.description.to_lowercase().contains("generation"),
                    "{} ({}) has zero context_window/max_output_tokens but does not appear \
                     to be a non-token model (description: {})",
                    entry.id,
                    entry.provider,
                    entry.description,
                );
                continue;
            }
            assert!(
                entry.context_window > 0,
                "{} has zero context_window",
                entry.id
            );
            assert!(
                entry.max_output_tokens > 0,
                "{} has zero max_output_tokens",
                entry.id
            );
        }
    }

    #[test]
    fn all_yaml_entries_have_at_least_text_modality() {
        for entry in yaml_catalog() {
            assert!(
                entry.input_modalities.contains(&InputModality::Text),
                "{} ({}) missing text modality",
                entry.id,
                entry.provider,
            );
        }
    }
}
