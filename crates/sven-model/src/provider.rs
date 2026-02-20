use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use crate::{CompletionRequest, ResponseEvent};

pub type ResponseStream = Pin<Box<dyn Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Human-readable provider name for status display.
    fn name(&self) -> &str;

    /// Model identifier as reported to users.
    fn model_name(&self) -> &str;

    /// Send a completion request and return a streaming response.
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream>;
}
