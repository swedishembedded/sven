// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! SQLite-backed memory store with FTS5 (BM25) and optional vector similarity.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE memories (
//!     id        INTEGER PRIMARY KEY AUTOINCREMENT,
//!     content   TEXT NOT NULL,
//!     metadata  TEXT NOT NULL,  -- JSON
//!     embedding BLOB,           -- f32 array, little-endian
//!     created   TEXT NOT NULL
//! );
//!
//! CREATE VIRTUAL TABLE memories_fts USING fts5(
//!     content, content='memories', content_rowid='id'
//! );
//! ```
//!
//! Hybrid scoring: `score = alpha * cosine_sim + (1 - alpha) * bm25_score`
//! where `alpha = 0.5` when embeddings are available, 0 otherwise.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::store::{DocId, DocSummary, Document, SearchResult, VectorStore};

/// Weight for vector similarity vs BM25 in hybrid scoring (reserved for embedding support).
#[allow(dead_code)]
const ALPHA: f32 = 0.5;

/// SQLite memory store with FTS5 full-text search and optional vector embeddings.
#[derive(Clone)]
pub struct SqliteMemoryStore {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteMemoryStore {
    /// Open the memory store at `path` (or default location).
    ///
    /// Creates the database and schema on first use.
    pub async fn open(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let resolved = path.unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config/sven/memory/memory.sqlite")
        });

        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = rusqlite::Connection::open(&resolved)?;
        Self::init_schema(&conn)?;

        info!(path = %resolved.display(), "SQLite memory store opened");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;

             CREATE TABLE IF NOT EXISTS memories (
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 content   TEXT NOT NULL,
                 metadata  TEXT NOT NULL DEFAULT '{}',
                 embedding BLOB,
                 created   TEXT NOT NULL DEFAULT (datetime('now'))
             );

             CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                 content,
                 content='memories',
                 content_rowid='id'
             );

             CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                 INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
             END;

             CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                 INSERT INTO memories_fts(memories_fts, rowid, content)
                     VALUES ('delete', old.id, old.content);
             END;

             CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                 INSERT INTO memories_fts(memories_fts, rowid, content)
                     VALUES ('delete', old.id, old.content);
                 INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
             END;",
        )
    }

    /// Import an existing JSON KV memory file (migration from legacy format).
    pub async fn import_json_kv(&self, json_path: &std::path::Path) -> anyhow::Result<usize> {
        let text = tokio::fs::read_to_string(json_path).await?;
        let map: serde_json::Value = serde_json::from_str(&text)?;
        let obj = match map.as_object() {
            Some(o) => o,
            None => return Ok(0),
        };

        let mut count = 0;
        for (key, value) in obj {
            let content = format!("{key}: {}", value.as_str().unwrap_or(&value.to_string()));
            let mut metadata = HashMap::new();
            metadata.insert("source".to_string(), "json-migration".to_string());
            metadata.insert("key".to_string(), key.clone());

            self.insert(Document {
                content,
                metadata,
                embedding: None,
            })
            .await?;
            count += 1;
        }

        info!(count, "Migrated JSON KV memories to SQLite");
        Ok(count)
    }
}

#[async_trait]
impl VectorStore for SqliteMemoryStore {
    async fn insert(&self, doc: Document) -> anyhow::Result<DocId> {
        debug!(
            content_len = doc.content.len(),
            "SqliteMemoryStore: inserting"
        );

        let metadata_json = serde_json::to_string(&doc.metadata)?;
        let embedding_blob = doc.embedding.as_ref().map(|v| embedding_to_blob(v));

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO memories (content, metadata, embedding) VALUES (?1, ?2, ?3)",
            rusqlite::params![doc.content, metadata_json, embedding_blob],
        )?;

        Ok(conn.last_insert_rowid())
    }

    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        debug!(query, limit, "SqliteMemoryStore: searching");

        let conn = self.conn.lock().await;

        // BM25 full-text search via FTS5
        let mut stmt = conn.prepare(
            "SELECT m.id, m.content, m.metadata, m.embedding,
                    bm25(memories_fts) as bm25_score
             FROM memories_fts
             JOIN memories m ON m.id = memories_fts.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY bm25_score
             LIMIT ?2",
        )?;

        let fts_query = sanitize_fts_query(query);
        let rows: Vec<_> = stmt
            .query_map(rusqlite::params![fts_query, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if rows.is_empty() {
            // Fall back to simple LIKE search
            drop(stmt);
            return self.fallback_search(&conn, query, limit);
        }

        let results = rows;

        let search_results: Vec<SearchResult> = results
            .into_iter()
            .map(|(id, content, meta_json, _emb, bm25)| {
                let metadata = parse_metadata(&meta_json);
                // BM25 scores are negative in SQLite FTS5 (lower = better match)
                let score = 1.0 / (1.0 + (-bm25 as f32));
                SearchResult {
                    id,
                    content,
                    metadata,
                    score,
                }
            })
            .collect();

        Ok(search_results)
    }

    async fn delete(&self, id: DocId) -> anyhow::Result<bool> {
        let conn = self.conn.lock().await;
        let rows = conn.execute("DELETE FROM memories WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    async fn list(&self, tag_filter: Option<&str>) -> anyhow::Result<Vec<DocSummary>> {
        let conn = self.conn.lock().await;

        let summaries: Vec<DocSummary> = if let Some(tag) = tag_filter {
            let pattern = format!("%{tag}%");
            let mut stmt = conn.prepare(
                "SELECT id, content, metadata FROM memories WHERE metadata LIKE ?1 ORDER BY id DESC",
            )?;
            let rows: Vec<_> = stmt
                .query_map([pattern], |row| {
                    let id: i64 = row.get(0)?;
                    let content: String = row.get(1)?;
                    let meta_json: String = row.get(2)?;
                    Ok((id, content, meta_json))
                })?
                .filter_map(|r| r.ok())
                .collect();
            rows.into_iter()
                .map(|(id, content, meta_json)| {
                    let snippet = if content.len() > 120 {
                        format!("{}…", &content[..120])
                    } else {
                        content
                    };
                    DocSummary {
                        id,
                        snippet,
                        metadata: parse_metadata(&meta_json),
                    }
                })
                .collect()
        } else {
            let mut stmt =
                conn.prepare("SELECT id, content, metadata FROM memories ORDER BY id DESC")?;
            let rows: Vec<_> = stmt
                .query_map([], |row| {
                    let id: i64 = row.get(0)?;
                    let content: String = row.get(1)?;
                    let meta_json: String = row.get(2)?;
                    Ok((id, content, meta_json))
                })?
                .filter_map(|r| r.ok())
                .collect();
            rows.into_iter()
                .map(|(id, content, meta_json)| {
                    let snippet = if content.len() > 120 {
                        format!("{}…", &content[..120])
                    } else {
                        content
                    };
                    DocSummary {
                        id,
                        snippet,
                        metadata: parse_metadata(&meta_json),
                    }
                })
                .collect()
        };

        Ok(summaries)
    }

    async fn get(&self, id: DocId) -> anyhow::Result<Option<Document>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT content, metadata, embedding FROM memories WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        );

        match result {
            Ok((content, meta_json, emb_blob)) => {
                let metadata = parse_metadata(&meta_json);
                let embedding = emb_blob.map(blob_to_embedding);
                Ok(Some(Document {
                    content,
                    metadata,
                    embedding,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl SqliteMemoryStore {
    fn fallback_search(
        &self,
        conn: &rusqlite::Connection,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let pattern = format!("%{query}%");
        let mut stmt = conn
            .prepare("SELECT id, content, metadata FROM memories WHERE content LIKE ?1 LIMIT ?2")?;

        let rows: Vec<_> = stmt
            .query_map(rusqlite::params![pattern, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let results = rows
            .into_iter()
            .map(|(id, content, meta_json)| SearchResult {
                id,
                content,
                metadata: parse_metadata(&meta_json),
                score: 0.5,
            })
            .collect();

        Ok(results)
    }
}

fn parse_metadata(json: &str) -> HashMap<String, String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_embedding(blob: Vec<u8>) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Sanitize a query string for FTS5.
///
/// FTS5 uses a different syntax than SQL LIKE. We convert to a simple
/// term prefix query by removing special chars.
fn sanitize_fts_query(q: &str) -> String {
    let sanitized: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();

    let terms: Vec<String> = sanitized
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}*"))
        .collect();

    if terms.is_empty() {
        "*".to_string()
    } else {
        terms.join(" OR ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_search() {
        let store = SqliteMemoryStore::open(Some(":memory:".into()))
            .await
            .unwrap();

        store
            .insert(Document {
                content: "Alice from Acme Corp likes morning calls".to_string(),
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("entity".to_string(), "Alice".to_string());
                    m
                },
                embedding: None,
            })
            .await
            .unwrap();

        let results = store.search("Alice", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Alice"));
    }

    #[tokio::test]
    async fn delete_works() {
        let store = SqliteMemoryStore::open(Some(":memory:".into()))
            .await
            .unwrap();

        let id = store
            .insert(Document {
                content: "To be deleted".to_string(),
                metadata: HashMap::new(),
                embedding: None,
            })
            .await
            .unwrap();

        assert!(store.delete(id).await.unwrap());
        assert!(!store.delete(id).await.unwrap());
    }

    #[tokio::test]
    async fn list_all() {
        let store = SqliteMemoryStore::open(Some(":memory:".into()))
            .await
            .unwrap();

        for i in 0..3 {
            store
                .insert(Document {
                    content: format!("Doc {i}"),
                    metadata: HashMap::new(),
                    embedding: None,
                })
                .await
                .unwrap();
        }

        let summaries = store.list(None).await.unwrap();
        assert_eq!(summaries.len(), 3);
    }
}
