// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Semantic memory store for sven agents.
//!
//! Provides a [`VectorStore`] trait backed by SQLite + FTS5 (BM25 text search)
//! with optional embedding vectors for semantic similarity search.
//!
//! # Design
//!
//! ```text
//! ┌──────────────────────────┐   ┌──────────────────────────┐
//! │    SemanticMemoryTool    │   │    AssimilateFactTool    │
//! │ remember|recall|forget…  │   │ the one knowledge writer │
//! └────────────┬─────────────┘   └───────┬──────────┬───────┘
//!              │                         │          │
//!              ▼                         ▼          ▼
//! ┌──────────────────────────────────────────┐ ┌──────────────────────┐
//! │             SqliteMemoryStore            │ │  PendingFactsLedger  │
//! │  SQLite + FTS5 (BM25) + cosine vectors   │ │ hash-chained, gated  │
//! └──────────────────────────────────────────┘ └──────────────────────┘
//!            session recall, always                durable, provenance-gated
//!                                                             │
//!                                                             ▼
//!                                                ┌──────────────────────────┐
//!                                                │    PendingFactsDrain     │
//!                                                │ batch → FactSubmitter →  │
//!                                                │ per-fact outcomes        │
//!                                                └──────────────────────────┘
//!                                                  what actually gets trained
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use sven_memory::{SqliteMemoryStore, Document, VectorStore};
//! use std::collections::HashMap;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let store = SqliteMemoryStore::open(None).await?;
//!
//! store.insert(Document {
//!     content: "Alice from Acme Corp prefers afternoon calls.".to_string(),
//!     metadata: {
//!         let mut m = HashMap::new();
//!         m.insert("source".to_string(), "contacts".to_string());
//!         m.insert("entity".to_string(), "Alice".to_string());
//!         m
//!     },
//!     embedding: None,
//! }).await?;
//!
//! let results = store.search("Alice contact preferences", 5).await?;
//! for r in results {
//!     println!("{}: {}", r.score, r.content);
//! }
//! # Ok(())
//! # }
//! ```

pub mod assimilate;
pub mod drain;
pub mod ingest;
pub mod ledger;
pub mod recall;
pub mod sqlite;
pub mod store;
pub mod tool;

pub use assimilate::{AssimilateFactTool, ProvenanceIndex};
pub use drain::{DrainError, FactOutcome, FactReport, FactSubmitter, PendingFactsDrain};
pub use ingest::IngestDocumentTool;
pub use ledger::{
    DocumentRecord, FrozenProbe, LedgerEntry, LedgerError, PendingFactRecord, PendingFactsLedger,
};
pub use recall::SessionScope;
pub use sqlite::SqliteMemoryStore;
pub use store::{DocId, DocSummary, Document, SearchResult, VectorStore};
pub use tool::SemanticMemoryTool;
