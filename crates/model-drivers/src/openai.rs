// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OpenAI driver - thin wrapper around the shared [`OpenAICompatProvider`].
//!
//! Kept as a named type so that the public `sven_model::OpenAiProvider` export
//! remains stable.

use async_trait::async_trait;

use crate::openai_compat::{AuthStyle, OpenAICompatProvider};
use sven_model::{catalog::ModelCatalogEntry, CompletionRequest, ResponseStream};

/// OpenAI chat-completions driver.
pub struct OpenAiProvider {
    inner: OpenAICompatProvider,
}

impl OpenAiProvider {
    pub fn new(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        driver_options: serde_json::Value,
    ) -> Self {
        Self {
            inner: OpenAICompatProvider::new(
                "openai",
                model,
                api_key,
                base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
                max_tokens,
                temperature,
                vec![],
                AuthStyle::Bearer,
                driver_options,
            ),
        }
    }
}

#[async_trait]
impl sven_model::ModelProvider for OpenAiProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        self.inner.list_models().await
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.inner.complete(req).await
    }

    async fn probe_context_window(&self) -> Option<u32> {
        // Without this delegation the trait default (`None`) silently wins
        // and OpenAiProvider can never discover a live context window, even
        // though the inner OpenAICompatProvider fully implements it - a real
        // hosted OpenAI endpoint has neither /props nor a per-model
        // context_length, so this still correctly resolves to None there;
        // it only matters when `base_url` is overridden to point at a local
        // OpenAI-compatible server (exactly what `--model openai` + a custom
        // base_url in config is for).
        self.inner.probe_context_window().await
    }
}
