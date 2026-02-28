// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! OpenAI driver — thin wrapper around the shared [`OpenAICompatProvider`].
//!
//! Kept as a named type so that the public `sven_model::OpenAiProvider` export
//! remains stable.

use async_trait::async_trait;

use crate::{
    catalog::ModelCatalogEntry,
    openai_compat::{AuthStyle, OpenAICompatProvider},
    provider::ResponseStream,
    CompletionRequest,
};

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
impl crate::ModelProvider for OpenAiProvider {
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
}
