use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use crate::{catalog::{InputModality, ModelCatalogEntry}, CompletionRequest, ResponseEvent};

pub type ResponseStream = Pin<Box<dyn Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

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

    /// Maximum output tokens for this provider/model combination.
    ///
    /// Reads from the static catalog; returns `None` if the model is unknown.
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        crate::catalog::lookup(self.name(), self.model_name())
            .map(|e| e.max_output_tokens)
    }

    /// Context window size for this provider/model combination.
    ///
    /// Reads from the static catalog; returns `None` if the model is unknown.
    fn catalog_context_window(&self) -> Option<u32> {
        crate::catalog::lookup(self.name(), self.model_name())
            .map(|e| e.context_window)
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
}
