// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The catalog of models: what the endpoints actually serve, remembered.
//!
//! Sven bundles no list of models, limits or prices. Every entry here was
//! reported by an endpoint's own `GET /models` (or its equivalent) and was put
//! here by the driver that listed it. A model that no endpoint has reported is
//! *unknown*: callers fall back to configuration, then to "no limit known".
//!
//! # Sources
//!
//! Every lookup function reads the **in-memory cache** of served models,
//! keyed by provider id. It is filled from two places:
//!
//! 1. **Disk cache** - a per-provider JSON file in
//!    `~/.config/sven/model-cache/<provider>.json` written by a previous run.
//!    Loaded at most once per provider per process ([`load_disk_cache`], or
//!    every file at once by [`load_all_disk_caches`]) and only while younger
//!    than 24 hours, so a start-up without network still knows the last served
//!    list.
//!
//! 2. **A successful listing** - a driver that lists its models calls
//!    [`cache_update`], which replaces the provider's entries in memory and on
//!    disk. A failed listing never touches the cache.

mod pricing;
mod served;
pub use pricing::{CachePricing, Pricing};
pub use served::{served_input_modalities, served_pricing_of, token_count};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use sven_rt::{SystemTime, UNIX_EPOCH};

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
    Audio,
}

fn default_input_modalities() -> Vec<InputModality> {
    // Conservative default: text only. A model is vision-capable only when its
    // endpoint says so.
    vec![InputModality::Text]
}

/// What an endpoint serves about one model. Every figure the endpoint did not
/// report is `None` - unknown, never a placeholder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCatalogEntry {
    /// Provider-scoped model identifier (e.g. "gpt-4o", "claude-opus-4-6")
    pub id: String,
    /// Human-readable display name; the id when the endpoint gives none.
    pub name: String,
    /// Provider identifier of the endpoint that served it.
    pub provider: String,
    /// Total context window in tokens (input + output).
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Maximum output tokens per completion.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Short description
    #[serde(default)]
    pub description: String,
    /// Supported input modalities.  Defaults to `[text]`.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    /// What the endpoint charges, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    /// Whether a call to it keeps no data anywhere, when the endpoint says
    /// (the sven service's `zero_data_retention`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_data_retention: Option<bool>,
}

impl ModelCatalogEntry {
    /// An entry that carries only what a bare model list gives: the id.
    #[must_use]
    pub fn bare(provider: &str, id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            context_window: None,
            max_output_tokens: None,
            description: String::new(),
            input_modalities: default_input_modalities(),
            pricing: None,
            zero_data_retention: None,
        }
    }

    /// Return `true` if the model can accept image input.
    pub fn supports_images(&self) -> bool {
        self.input_modalities.contains(&InputModality::Image)
    }

    /// Return `true` if the model can accept audio input.
    pub fn supports_audio(&self) -> bool {
        self.input_modalities.contains(&InputModality::Audio)
    }
}

// ── In-memory cache ───────────────────────────────────────────────────────────

/// Global in-memory cache of served models keyed by provider id.
///
/// - Populated from disk on first access per provider ([`load_disk_cache`]).
/// - Replaced by a driver's successful listing ([`cache_update`]).
fn served_cache() -> &'static RwLock<HashMap<String, Vec<ModelCatalogEntry>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Vec<ModelCatalogEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Tracks which providers have already had their disk cache loaded this run.
fn disk_loaded() -> &'static Mutex<HashSet<String>> {
    static LOADED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOADED.get_or_init(|| Mutex::new(HashSet::new()))
}

// ── Disk cache helpers ────────────────────────────────────────────────────────

/// On-disk serialisation for a single provider's served-model cache.
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

fn cache_dir() -> Option<PathBuf> {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|p| p.join("sven").join("model-cache"))
}

fn disk_cache_path(provider: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{provider}.json")))
}

/// The entries of the cache file at `path`; `None` when it is absent,
/// unreadable, invalid or older than `CACHE_TTL_SECS`.
fn read_fresh_cache_file(path: &Path) -> Option<Vec<ModelCatalogEntry>> {
    let dc: DiskCache = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if current_unix_secs().saturating_sub(dc.fetched_at) > CACHE_TTL_SECS {
        return None;
    }
    Some(dc.entries)
}

fn write_cache_file(path: &Path, entries: &[ModelCatalogEntry]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dc = DiskCache {
        fetched_at: current_unix_secs(),
        entries: entries.to_vec(),
    };
    std::fs::write(path, serde_json::to_vec_pretty(&dc)?)?;
    Ok(())
}

// ── Public cache API ──────────────────────────────────────────────────────────

/// Load the on-disk cache for `provider` into memory.
///
/// Called at most once per provider per process (guarded by `disk_loaded`).
/// Does nothing when the file is absent, unreadable, invalid JSON, or older
/// than `CACHE_TTL_SECS`, and never replaces entries a listing already put in
/// memory this run.
pub fn load_disk_cache(provider: &str) {
    {
        let mut set = disk_loaded().lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(provider.to_string()) {
            return;
        }
    }
    let Some(entries) = disk_cache_path(provider).and_then(|p| read_fresh_cache_file(&p)) else {
        return;
    };
    if let Ok(mut guard) = served_cache().write() {
        guard.entry(provider.to_string()).or_insert(entries);
    }
}

/// Load the on-disk cache of every provider that has one (see
/// [`load_disk_cache`]). For callers that look across providers.
pub fn load_all_disk_caches() {
    let Some(dir) = cache_dir() else { return };
    let Ok(files) = std::fs::read_dir(dir) else {
        return;
    };
    for path in files.filter_map(|f| f.ok().map(|f| f.path())) {
        if path.extension().is_some_and(|e| e == "json") {
            if let Some(provider) = path.file_stem().and_then(|s| s.to_str()) {
                load_disk_cache(provider);
            }
        }
    }
}

/// Return `true` when the on-disk cache for `provider` is absent or expired.
pub fn is_cache_stale(provider: &str) -> bool {
    disk_cache_path(provider)
        .and_then(|p| read_fresh_cache_file(&p))
        .is_none()
}

/// Replace the cached models of `provider` with what its endpoint just
/// served, in memory and on disk.
///
/// Called by a driver after a **successful** listing. An empty list is
/// ignored: an endpoint that answers with no models has told us nothing worth
/// forgetting what the last answer said.
pub fn cache_update(provider: &str, entries: Vec<ModelCatalogEntry>) {
    if entries.is_empty() {
        return;
    }
    if let Some(path) = disk_cache_path(provider) {
        // Best effort: a read-only config directory only costs the next
        // start-up its warm cache.
        let _ = write_cache_file(&path, &entries);
    }
    if let Ok(mut guard) = served_cache().write() {
        guard.insert(provider.to_string(), entries);
    }
}

/// Add `entries` to the in-memory cache of their provider, replacing any with
/// the same id, without touching the disk. For tests and for an embedding
/// application that already knows what its endpoint serves.
pub fn cache_seed(entries: impl IntoIterator<Item = ModelCatalogEntry>) {
    let Ok(mut guard) = served_cache().write() else {
        return;
    };
    for entry in entries {
        let list = guard.entry(entry.provider.clone()).or_default();
        list.retain(|e| e.id != entry.id);
        list.push(entry);
    }
}

// ── Lookup API ────────────────────────────────────────────────────────────────

/// Every model any endpoint has served, ordered by provider then id.
pub fn served_models() -> Vec<ModelCatalogEntry> {
    load_all_disk_caches();
    let mut all: Vec<ModelCatalogEntry> = served_cache()
        .read()
        .map(|guard| guard.values().flatten().cloned().collect())
        .unwrap_or_default();
    all.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));
    all
}

/// Look up a single model by provider and id (or name). `None` when no
/// endpoint of that provider has served it.
pub fn lookup(provider: &str, model_id: &str) -> Option<ModelCatalogEntry> {
    load_disk_cache(provider);
    served_cache()
        .read()
        .ok()?
        .get(provider)?
        .iter()
        .find(|e| e.id == model_id || e.name == model_id)
        .cloned()
}

/// Look up a model by bare model name (without a provider prefix).
///
/// Searches `id` and `name` of every provider's served models and returns the
/// first match in provider order.
///
/// Used by `ModelResolver` step 3 to detect when a bare model name (e.g.
/// `"gpt-4o"`) can be resolved against the provider that served it.
pub fn lookup_by_model_name(model_name: &str) -> Option<ModelCatalogEntry> {
    served_models()
        .into_iter()
        .find(|e| e.id == model_name || e.name == model_name)
}

/// The price an endpoint served for `model_id` at `provider`; `None` when it
/// served none, which means unknown and never free.
pub fn served_pricing(provider: &str, model_id: &str) -> Option<Pricing> {
    lookup(provider, model_id)?.pricing
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests seed under provider names no real driver uses, so a developer's
    /// own disk cache cannot collide with them.
    fn entry(provider: &str, id: &str) -> ModelCatalogEntry {
        ModelCatalogEntry {
            context_window: Some(131_072),
            max_output_tokens: Some(8_192),
            ..ModelCatalogEntry::bare(provider, id)
        }
    }

    #[test]
    fn a_seeded_model_is_found_by_provider_and_by_bare_name() {
        cache_seed([entry("seed-a", "model-a")]);
        let found = lookup("seed-a", "model-a").unwrap();
        assert_eq!(found.context_window, Some(131_072));
        assert_eq!(lookup_by_model_name("model-a").unwrap().provider, "seed-a");
        assert!(served_models().iter().any(|e| e.id == "model-a"));
    }

    #[test]
    fn a_model_no_endpoint_served_is_unknown_with_no_window_and_no_price() {
        assert!(lookup("seed-b", "nonexistent-model-xyz").is_none());
        assert!(lookup_by_model_name("nonexistent-model-xyz").is_none());
        assert!(served_pricing("seed-b", "nonexistent-model-xyz").is_none());
    }

    #[test]
    fn a_served_price_is_found_and_a_model_served_without_one_has_none() {
        let priced = Pricing {
            input_per_mtok: 0.14,
            output_per_mtok: 0.42,
            cache: None,
        };
        cache_seed([
            ModelCatalogEntry {
                pricing: Some(priced),
                ..entry("seed-c", "priced")
            },
            entry("seed-c", "unpriced"),
        ]);
        assert_eq!(served_pricing("seed-c", "priced"), Some(priced));
        assert_eq!(served_pricing("seed-c", "unpriced"), None);
    }

    #[test]
    fn seeding_replaces_an_entry_of_the_same_id_and_keeps_the_others() {
        cache_seed([entry("seed-d", "one"), entry("seed-d", "two")]);
        cache_seed([ModelCatalogEntry {
            context_window: Some(1),
            ..entry("seed-d", "one")
        }]);
        assert_eq!(lookup("seed-d", "one").unwrap().context_window, Some(1));
        assert!(lookup("seed-d", "two").is_some());
    }

    #[test]
    fn the_disk_cache_round_trips_and_expires_after_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        write_cache_file(&path, &[entry("p", "m")]).unwrap();
        assert_eq!(read_fresh_cache_file(&path).unwrap()[0].id, "m");

        let stale = DiskCache {
            fetched_at: current_unix_secs() - CACHE_TTL_SECS - 1,
            entries: vec![entry("p", "m")],
        };
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(read_fresh_cache_file(&path).is_none());
    }

    #[test]
    fn a_missing_or_corrupt_cache_file_reads_as_no_cache() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_fresh_cache_file(&dir.path().join("absent.json")).is_none());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert!(read_fresh_cache_file(&bad).is_none());
    }
}
