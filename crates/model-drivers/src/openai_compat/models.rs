// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A `GET /models` response read into catalog entries: the rich metadata
//! OpenRouter returns, and bare ids enriched from the static catalog for every
//! other OpenAI-compatible server.

use serde_json::Value;
use sven_model::catalog::{InputModality, ModelCatalogEntry};

/// Parse a `GET /models` response body into a list of [`ModelCatalogEntry`].
///
/// For OpenRouter, uses the rich metadata the API returns (`context_length`,
/// `top_provider.max_completion_tokens`, `architecture.input_modalities`).
/// For all other OpenAI-compatible providers, enriches bare IDs with any
/// matching static-catalog entry; unknown IDs get zeroed placeholders.
///
/// Any static-catalog entries that are not in the live response (e.g. the
/// OpenRouter synthetic `openrouter/auto` and `openrouter/free` meta-models)
/// are appended so they remain discoverable.
pub(crate) fn parse_models_response(
    body: &Value,
    driver_name: &str,
    catalog_entries: &[ModelCatalogEntry],
) -> Vec<ModelCatalogEntry> {
    let Some(data) = body["data"].as_array() else {
        return Vec::new();
    };

    let mut entries: Vec<ModelCatalogEntry> = data
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let entry = if driver_name == "openrouter" {
                parse_openrouter_model_item(item, id)
            } else if let Some(cat) = catalog_entries.iter().find(|e| e.id == id) {
                cat.clone()
            } else {
                ModelCatalogEntry {
                    id: id.clone(),
                    name: id,
                    provider: driver_name.to_string(),
                    context_window: 0,
                    max_output_tokens: 0,
                    description: String::new(),
                    input_modalities: vec![InputModality::Text],
                }
            };
            Some(entry)
        })
        .collect();

    // Append static-only entries not present in the live response (meta-models,
    // custom local aliases, etc.).
    let live_ids: std::collections::HashSet<String> =
        entries.iter().map(|e| e.id.clone()).collect();
    for cat in catalog_entries {
        if !live_ids.contains(&cat.id) {
            entries.push(cat.clone());
        }
    }

    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

/// Parse a single model item from the OpenRouter `/models` API response.
///
/// Extracts the rich metadata OpenRouter provides and maps it to our
/// [`ModelCatalogEntry`] fields.
pub(crate) fn parse_openrouter_model_item(item: &Value, id: String) -> ModelCatalogEntry {
    let name = item["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();
    let description = item["description"].as_str().unwrap_or("").to_string();
    let context_window = item["context_length"].as_u64().unwrap_or(0) as u32;
    let max_output_tokens = item["top_provider"]["max_completion_tokens"]
        .as_u64()
        .unwrap_or(0) as u32;
    let input_modalities =
        parse_openrouter_input_modalities(item["architecture"]["input_modalities"].as_array());
    ModelCatalogEntry {
        id,
        name,
        provider: "openrouter".to_string(),
        context_window,
        max_output_tokens,
        description,
        input_modalities,
    }
}

/// Map the `architecture.input_modalities` string array from the OpenRouter
/// `/models` response to our [`InputModality`] enum values.
fn parse_openrouter_input_modalities(arr: Option<&Vec<Value>>) -> Vec<InputModality> {
    let mut result = vec![InputModality::Text];
    let Some(arr) = arr else { return result };
    for v in arr {
        if v.as_str() == Some("image") && !result.contains(&InputModality::Image) {
            result.push(InputModality::Image);
        }
    }
    result
}
