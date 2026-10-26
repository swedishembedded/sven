// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `ocr` section of the configuration file.
//!
//! Swedish Embedded AB implements document conversion pipelines for its
//! clients. If your team needs expertise in turning scanned archives into
//! searchable Markdown then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sven_config::Schema;

/// One or several model specs: several are replicas of the same model, e.g.
/// one server per GPU, and share the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Specs {
    One(String),
    Many(Vec<String>),
}

impl Specs {
    pub fn as_slice(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    /// The OCR model `document_convert` and `sven document convert` use when
    /// none is named: a spec, an `models` alias or a loose name. Without
    /// it the session's model is used if it accepts images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Named OCR models: alias to spec, or to a list of specs that serve the
    /// same model on several endpoints.
    pub models: BTreeMap<String, Specs>,
    /// Model requests in flight at once (the ceiling when concurrency adapts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    /// Render resolution in dots per inch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f32>,
    /// Output-token limit per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// `auto`, `off` or `regions`: whether pages are read block by block from
    /// their layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// The instruction sent with each page image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl OcrConfig {
    /// The keys of the `ocr` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "model",
            "concurrency",
            "dpi",
            "max_tokens",
            "layout",
            "prompt",
        ])
        .with("models", Schema::entries(Schema::value()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_take_one_spec_or_a_replica_list() {
        let c: OcrConfig = serde_yaml::from_str(
            "model: fast\nmodels:\n  fast: brain/deepseek-ai/DeepSeek-OCR\n  pool: [a/m, b/m]\ndpi: 200\n",
        )
        .unwrap();
        assert_eq!(c.models["pool"].as_slice(), vec!["a/m", "b/m"]);
        assert_eq!(c.dpi, Some(200.0));
        assert_eq!(c.max_tokens, None);
    }
}
