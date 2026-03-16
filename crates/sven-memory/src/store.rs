// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! VectorStore trait and associated types.

use std::collections::HashMap;

use async_trait::async_trait;

/// Opaque document identifier.
pub type DocId = i64;

/// A memory document.
#[derive(Debug, Clone)]
pub struct Document {
    /// Plain-text content to store and search.
    pub content: String,
    /// Key-value metadata (source, entity, tags, date, etc.).
    pub metadata: HashMap<String, String>,
    /// Optional pre-computed embedding vector.
    ///
    /// When `None`, the store may compute the embedding if an embedding
    /// provider is configured, or fall back to BM25-only retrieval.
    pub embedding: Option<Vec<f32>>,
}

/// A search result from the memory store.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Document identifier.
    pub id: DocId,
    /// Document content.
    pub content: String,
    /// Metadata key-value pairs.
    pub metadata: HashMap<String, String>,
    /// Relevance score (higher is more relevant; scale is implementation-specific).
    pub score: f32,
}

/// A brief document summary (for listing without full content).
#[derive(Debug, Clone)]
pub struct DocSummary {
    /// Document identifier.
    pub id: DocId,
    /// First 120 characters of content.
    pub snippet: String,
    /// Metadata.
    pub metadata: HashMap<String, String>,
}

/// Semantic memory store trait.
///
/// Implementations must provide insertion, semantic search, deletion, and listing.
/// The store is `Clone + Send + Sync` so it can be shared across tasks.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Insert a document into the store.
    ///
    /// Returns the assigned [`DocId`].
    async fn insert(&self, doc: Document) -> anyhow::Result<DocId>;

    /// Search for documents semantically similar to `query`.
    ///
    /// Returns up to `limit` results ordered by relevance (descending).
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>>;

    /// Delete a document by ID.
    ///
    /// Returns `true` if the document existed and was deleted.
    async fn delete(&self, id: DocId) -> anyhow::Result<bool>;

    /// List all documents (summaries only).
    ///
    /// An optional `tag` filter (matches any metadata value) narrows results.
    async fn list(&self, tag_filter: Option<&str>) -> anyhow::Result<Vec<DocSummary>>;

    /// Retrieve a single document by ID.
    async fn get(&self, id: DocId) -> anyhow::Result<Option<Document>>;
}
