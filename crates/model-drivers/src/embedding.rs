// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for retrieval over private agent
// memory for its clients. If your team needs expertise in semantic search that
// keeps tenants apart then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Embeddings from an OpenAI-compatible server (brain serve among them).

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_memory_api::{sha256_hex, Embedder, EmbeddingSpec, MemoryError, PromptStyle};

/// Embeds through `POST {base_url}/embeddings`.
///
/// The server does not report a weights digest, so the spec names the model
/// the way the server serves it and the endpoint it came from; a different
/// checkpoint behind the same name is not noticed here.
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    spec: EmbeddingSpec,
}

impl OpenAiEmbedder {
    /// Connects and learns the vector length by embedding one probe text.
    ///
    /// # Errors
    /// [`MemoryError::Unavailable`] if the server cannot be reached or answers
    /// something unusable.
    pub async fn connect(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let model = model.into();
        let spec = EmbeddingSpec {
            model_id: model.clone(),
            weights_digest: sha256_hex(format!("served:{base_url}:{model}").as_bytes()),
            tokenizer_digest: sha256_hex(format!("served-tokenizer:{model}").as_bytes()),
            pooling: "server".into(),
            normalized: false,
            dims: 0,
            prompt: PromptStyle::Plain,
            max_tokens: 8192,
        };
        let mut embedder = Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
            spec,
        };
        let probe = embedder.request(&["dimension probe".to_owned()]).await?;
        embedder.spec.dims = probe.first().map_or(0, Vec::len);
        if embedder.spec.dims == 0 {
            return Err(MemoryError::Unavailable(
                "the server answered the probe with an empty vector".into(),
            ));
        }
        Ok(embedder)
    }

    async fn request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        let unavailable = |why: String| MemoryError::Unavailable(format!("embeddings: {why}"));
        let mut call = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .json(&json!({"model": self.model, "input": texts}));
        if let Some(key) = &self.api_key {
            call = call.bearer_auth(key);
        }
        let response = call.send().await.map_err(|e| unavailable(e.to_string()))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        if !status.is_success() {
            return Err(unavailable(format!("status {status}: {body}")));
        }
        let data = body["data"]
            .as_array()
            .ok_or_else(|| unavailable("no data in the answer".into()))?;
        if data.len() != texts.len() {
            return Err(unavailable(format!(
                "{} vectors for {} texts",
                data.len(),
                texts.len()
            )));
        }
        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for item in data {
            let index = item["index"]
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .filter(|i| *i < texts.len())
                .ok_or_else(|| unavailable("a vector without a usable index".into()))?;
            let vector: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| unavailable("a vector that is not a list".into()))?
                .iter()
                .map(|v| v.as_f64().map(|f| f as f32))
                .collect::<Option<_>>()
                .ok_or_else(|| unavailable("a vector with a non-number".into()))?;
            if vectors[index].replace(vector).is_some() {
                return Err(unavailable("two vectors for one text".into()));
            }
        }
        vectors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unavailable("a text without a vector".into()))
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    fn spec(&self) -> EmbeddingSpec {
        self.spec.clone()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        let vectors = self.request(texts).await?;
        if vectors.iter().any(|v| v.len() != self.spec.dims) {
            return Err(MemoryError::Unavailable(format!(
                "embeddings: a vector is not {} values long",
                self.spec.dims
            )));
        }
        Ok(vectors)
    }
}
