// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How the tool obtains an OCR model without knowing about configuration.

use std::sync::Arc;

use async_trait::async_trait;
use sven_model::ModelProvider;

/// A resolved OCR model: its identity and the endpoints serving it.
#[derive(Clone)]
pub struct OcrModel {
    /// Names the model in the manifest and in page identities.
    pub spec: String,
    /// Interchangeable endpoints of that model (one in the common case).
    pub providers: Vec<Arc<dyn ModelProvider>>,
}

/// Why no OCR model could be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoOcrModel {
    /// Nothing usable is on offer: none configured and none discovered, or the
    /// configured one is unreachable or cannot take images. The caller must
    /// tell the user rather than work around it.
    Unavailable { reason: String },
    /// The request itself is wrong (an unknown or ambiguous name); the
    /// message lists the choices.
    Refused(String),
}

/// Resolves a requested model (`None`: the configured default) to an
/// image-capable [`OcrModel`].
#[async_trait]
pub trait OcrModels: Send + Sync {
    async fn resolve(&self, requested: Option<&str>) -> Result<OcrModel, NoOcrModel>;
}
