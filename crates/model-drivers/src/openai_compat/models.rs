// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A `GET /models` response read into catalog entries: whatever the server
//! reports about each model, and nothing it does not.

use serde_json::Value;
use sven_model::catalog::ModelCatalogEntry;

/// Parse a `GET /models` response body into the entries `driver_name`'s server
/// serves, ordered by id. Empty when the body is not a model list.
///
/// Limits, modalities and prices are read as [`ModelCatalogEntry::from_served`]
/// documents; a field the server leaves out stays unknown.
pub(crate) fn parse_models_response(body: &Value, driver_name: &str) -> Vec<ModelCatalogEntry> {
    let Some(data) = body["data"].as_array() else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|item| ModelCatalogEntry::from_served(driver_name, item))
        .collect()
}
