//! Model catalog: static metadata for known models, with optional live refresh.

use serde::{Deserialize, Serialize};

/// Metadata for a single model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    /// Provider-scoped model identifier (e.g. "gpt-4o", "claude-opus-4-5")
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
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    models: Vec<ModelCatalogEntry>,
}

/// Return all entries from the bundled static catalog.
pub fn static_catalog() -> Vec<ModelCatalogEntry> {
    let yaml = include_str!("../models.yaml");
    let catalog: CatalogFile =
        serde_yaml::from_str(yaml).expect("bundled models.yaml must be valid");
    catalog.models
}

/// Look up a single model by provider and id (or name).
/// Returns `None` if not found in the static catalog.
pub fn lookup(provider: &str, model_id: &str) -> Option<ModelCatalogEntry> {
    static_catalog()
        .into_iter()
        .find(|e| e.provider == provider && (e.id == model_id || e.name == model_id))
}

/// Look up the context window for a model.  Falls back to `default` if not in catalog.
pub fn context_window(provider: &str, model_id: &str, default: u32) -> u32 {
    lookup(provider, model_id)
        .map(|e| e.context_window)
        .unwrap_or(default)
}

/// Look up the max output tokens for a model.  Falls back to `default` if not in catalog.
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
        assert!(!models.is_empty(), "bundled catalog must contain at least one model");
    }

    #[test]
    fn gpt4o_is_in_catalog() {
        let entry = lookup("openai", "gpt-4o").expect("gpt-4o must be in catalog");
        assert_eq!(entry.provider, "openai");
        assert!(entry.context_window >= 128_000);
        assert!(entry.max_output_tokens >= 4_096);
    }

    #[test]
    fn claude_opus_is_in_catalog() {
        let entry = lookup("anthropic", "claude-opus-4-5").expect("claude-opus-4-5 must be in catalog");
        assert_eq!(entry.provider, "anthropic");
        assert!(entry.context_window >= 200_000);
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
    fn all_entries_have_non_zero_windows() {
        for entry in static_catalog() {
            assert!(entry.context_window > 0, "{} has zero context_window", entry.id);
            assert!(entry.max_output_tokens > 0, "{} has zero max_output_tokens", entry.id);
        }
    }
}
