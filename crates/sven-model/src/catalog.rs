// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Model catalog: static metadata for known models, with optional live refresh.

use serde::{Deserialize, Serialize};

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

/// Look up a model by bare model name (without provider prefix).
///
/// Checks `id` and `name` fields.  Returns the first matching entry from the
/// static catalog or `None` if not found.
///
/// Used by `resolve_model_from_config` to detect when a bare model name (e.g.
/// `"gpt-4o"`) should be resolved against the catalog provider rather than
/// inheriting the custom `base_url` from the user's config.
pub fn lookup_by_model_name(model_name: &str) -> Option<ModelCatalogEntry> {
    static_catalog()
        .into_iter()
        .find(|e| e.id == model_name || e.name == model_name)
}

/// Return `true` if the model supports image input, defaulting to `false` when
/// the model is not found in the catalog.
pub fn supports_images(provider: &str, model_id: &str) -> bool {
    lookup(provider, model_id)
        .map(|e| e.supports_images())
        .unwrap_or(false)
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
    fn all_entries_have_non_zero_windows() {
        for entry in static_catalog() {
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
    fn all_entries_have_at_least_text_modality() {
        for entry in static_catalog() {
            assert!(
                entry.input_modalities.contains(&InputModality::Text),
                "{} ({}) missing text modality",
                entry.id,
                entry.provider,
            );
        }
    }
}
