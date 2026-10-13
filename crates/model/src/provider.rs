// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use crate::{
    catalog::{InputModality, ModelCatalogEntry},
    CompletionRequest, ResponseEvent,
};

pub type ResponseStream = Pin<Box<dyn Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

/// The adapter (fine-tune overlay) a server currently has applied to a model.
///
/// `digest` changes whenever the adapter's weights change, so comparing two
/// probes' digests detects a hot swap even when `id` stays the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRef {
    pub id: String,
    pub digest: String,
}

/// One model a server reports it is serving right now.
///
/// Every field except `id` is optional because servers differ in what they
/// advertise and older servers advertise nothing beyond the id. `None` always
/// means "not reported", never "no" - callers fall back to static catalog
/// metadata for those.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServedModel {
    pub id: String,
    /// Total context window the server loaded this model with, in tokens.
    pub context_length: Option<u32>,
    /// Largest completion the server will produce for this model, in tokens.
    pub max_output_tokens: Option<u32>,
    /// Whether the model supports tool calling: `Some` only when the server
    /// advertised a capability list, `None` when it said nothing.
    pub tools: Option<bool>,
    /// The raw capability names the server advertised, unfiltered.
    pub capabilities: Option<Vec<String>>,
    /// The adapter currently applied to this model, when the server reports one.
    pub adapter: Option<AdapterRef>,
}

/// What a server reports it is serving, in the order it reported it.
pub type ServedModels = Vec<ServedModel>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Human-readable provider name for status display.
    fn name(&self) -> &str;

    /// Model identifier as reported to users.
    fn model_name(&self) -> &str;

    /// Send a completion request and return a streaming response.
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream>;

    /// List all models available from this provider.
    ///
    /// The default implementation returns only the static catalog entries for
    /// this provider.  Override to perform a live API query (and then merge
    /// with the catalog for metadata enrichment).
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let provider = self.name();
        let entries = crate::catalog::static_catalog()
            .into_iter()
            .filter(|e| e.provider == provider)
            .collect();
        Ok(entries)
    }

    /// Ask the live server which models it is serving right now.
    ///
    /// Unlike [`Self::list_models`], which merges static catalog entries, this
    /// reports only what the server itself says, so a model loaded or an
    /// adapter swapped after sven started is visible. Returns `None` when the
    /// provider has no discovery endpoint or the server did not answer in
    /// time; it never fails a turn. The default implementation returns `None`.
    async fn discover(&self) -> Option<ServedModels> {
        None
    }

    /// The host (and port) of the server this provider talks to, for naming
    /// it in errors. `None` when the provider has no network endpoint.
    fn endpoint_host(&self) -> Option<String> {
        None
    }

    /// Whether the server runs on the user's own hardware, per the driver
    /// registry. A local server can take minutes to answer a long prompt, so
    /// its stall limits are more generous than a hosted API's.
    fn is_local(&self) -> bool {
        crate::registry::get_driver(self.name()).is_some_and(|driver| driver.local)
    }

    /// Maximum output tokens for this provider/model combination.
    ///
    /// Reads from the static catalog; returns `None` if the model is unknown.
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        crate::catalog::lookup(self.name(), self.model_name()).map(|e| e.max_output_tokens)
    }

    /// Context window size for this provider/model combination.
    ///
    /// Reads from the static catalog; returns `None` if the model is unknown.
    fn catalog_context_window(&self) -> Option<u32> {
        crate::catalog::lookup(self.name(), self.model_name()).map(|e| e.context_window)
    }

    /// Config-specified total context window (overrides catalog when set).
    ///
    /// Returns the `max_tokens` value from the user's config, which represents
    /// the total context window (input + output) for this model.  Used for
    /// compaction budget decisions.  Returns `None` when the user did not
    /// explicitly configure a context window; callers should then fall back to
    /// [`Self::catalog_context_window`] or a conservative default.
    fn config_context_window(&self) -> Option<u32> {
        None
    }

    /// Config-specified output token limit (overrides catalog when set).
    ///
    /// Returns the resolved output token cap derived from the user's config:
    /// - `max_output_tokens` if explicitly set, else
    /// - `max_tokens` (total) used as backward-compatible output cap.
    ///
    /// Returns `None` when neither is configured; callers should then fall back
    /// to [`Self::catalog_max_output_tokens`] or a conservative default.
    fn config_max_output_tokens(&self) -> Option<u32> {
        None
    }

    /// Query the live API for the actual context window in use.
    ///
    /// Default implementation returns `None` (no live probe available).
    /// Override in providers that expose a properties or info endpoint -
    /// e.g. llama.cpp-compatible servers expose `GET /props` which includes
    /// the loaded `n_ctx` value.
    ///
    /// Returns `Some(n_ctx)` when the probe succeeds, `None` otherwise.
    /// A `Some(0)` result is treated as "unknown" by callers.
    async fn probe_context_window(&self) -> Option<u32> {
        None
    }

    /// Input modalities supported by this provider/model combination.
    ///
    /// Reads from the static catalog.  Returns `[Text]` when the model is not
    /// found, to be conservative (avoid sending images to unknown models).
    fn input_modalities(&self) -> Vec<InputModality> {
        crate::catalog::lookup(self.name(), self.model_name())
            .map(|e| e.input_modalities)
            .unwrap_or_else(|| vec![InputModality::Text])
    }

    /// Returns `true` if this model supports image input.
    fn supports_images(&self) -> bool {
        self.input_modalities().contains(&InputModality::Image)
    }

    /// Returns `true` if this model supports audio input natively.
    ///
    /// Callers use this to decide between attaching raw audio and falling
    /// back to a text transcript (see `sven-tools-fs`' `attach_file` tool).
    fn supports_audio(&self) -> bool {
        self.input_modalities().contains(&InputModality::Audio)
    }
}
