// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! SQLite adapter for the [`sven_memory_api::MemoryStore`] contract.
//!
//! One database file holds every space. Isolation is a condition of each
//! query (the caller's spaces are bound into the `WHERE` clause), never a
//! filter over what comes back, so a result limit counts only rows the caller
//! may see. Lexical retrieval is FTS5/BM25 over units; embeddings are stored
//! per index generation. The schema is versioned in `user_version` and
//! migrated on open, and a file written by a newer build is refused.
//!
//! Swedish Embedded AB implements solutions for embedded, tenant-isolated
//! agent memory for its clients. If your team needs expertise in putting
//! searchable agent memory into a product on SQLite then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod migrate;
mod ops;
mod rows;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use rusqlite::Connection;
use sven_memory_api::{
    AccessContext, Annotation, EmbeddingSpec, GenerationId, GenerationState, IndexGeneration,
    IngestOutcome, IngestRequest, MemoryError, MemoryStore, NewAnnotation, RetrievalRequest,
    RetrievalResponse, RevisionId, SourceId, SourceSummary, SpaceId, Unit, UnitAnchor, UnitId,
};

pub use migrate::SCHEMA_VERSION;

/// Maps an engine failure to the contract's error, keeping the message.
pub(crate) fn storage(error: rusqlite::Error) -> MemoryError {
    MemoryError::Storage(error.to_string())
}

/// The location of the memory database below a home directory.
const DEFAULT_RELATIVE_PATH: &str = ".config/sven/memory/memory.sqlite";

/// A [`MemoryStore`] on one SQLite database.
///
/// Cheap to clone; clones share the connection, which serializes writers.
#[derive(Clone)]
pub struct SqliteMemory {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteMemory {
    /// Opens (creating if needed) the database at `path` and migrates it.
    ///
    /// # Errors
    /// [`MemoryError::Unavailable`] if the file cannot be opened or was
    /// written by a newer build.
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                MemoryError::Unavailable(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let conn = Connection::open(path).map_err(|e| {
            MemoryError::Unavailable(format!("cannot open {}: {e}", path.display()))
        })?;
        Self::prepare(conn, true)
    }

    /// A private database that lives as long as the returned store.
    ///
    /// # Errors
    /// [`MemoryError::Unavailable`] if SQLite cannot create it.
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| MemoryError::Unavailable(format!("cannot open in memory: {e}")))?;
        Self::prepare(conn, false)
    }

    /// The database path below `home`.
    ///
    /// There is no fallback location: without a home directory the caller must
    /// say where the database goes, and nothing is created.
    ///
    /// # Errors
    /// [`MemoryError::Unavailable`] if `home` is `None`.
    pub fn default_path(home: Option<&Path>) -> Result<PathBuf, MemoryError> {
        home.map(|home| home.join(DEFAULT_RELATIVE_PATH))
            .ok_or_else(|| {
                MemoryError::Unavailable(
                    "no home directory: pass an explicit database path".to_owned(),
                )
            })
    }

    /// Opens the database at the default location below `home`.
    ///
    /// # Errors
    /// See [`Self::default_path`] and [`Self::open`].
    pub fn open_in_home(home: Option<&Path>) -> Result<Self, MemoryError> {
        Self::open(&Self::default_path(home)?)
    }

    fn prepare(mut conn: Connection, durable: bool) -> Result<Self, MemoryError> {
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(storage)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage)?;
        if durable {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(storage)?;
            conn.pragma_update(None, "synchronous", "NORMAL")
                .map_err(storage)?;
        }
        migrate::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `operation` on a blocking thread with the connection locked.
    async fn run<T, F>(&self, operation: F) -> Result<T, MemoryError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, MemoryError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| MemoryError::Storage("the connection lock is poisoned".into()))?;
            operation(&mut guard)
        })
        .await
        .map_err(|e| MemoryError::Storage(format!("storage task failed: {e}")))?
    }
}

#[async_trait]
impl MemoryStore for SqliteMemory {
    async fn ingest(
        &self,
        ctx: &AccessContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, MemoryError> {
        let ctx = ctx.clone();
        self.run(move |conn| ops::ingest(conn, &ctx, &request))
            .await
    }

    async fn retrieve(
        &self,
        ctx: &AccessContext,
        request: RetrievalRequest,
    ) -> Result<RetrievalResponse, MemoryError> {
        let ctx = ctx.clone();
        self.run(move |conn| ops::retrieve(conn, &ctx, &request))
            .await
    }

    async fn get_unit(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit_id: &UnitId,
    ) -> Result<Option<Unit>, MemoryError> {
        let (ctx, space, unit_id) = (ctx.clone(), space.clone(), unit_id.clone());
        self.run(move |conn| ops::get_unit(conn, &ctx, &space, &unit_id))
            .await
    }

    async fn list_sources(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        tag: Option<&str>,
    ) -> Result<Vec<SourceSummary>, MemoryError> {
        let (ctx, space, tag) = (ctx.clone(), space.clone(), tag.map(str::to_owned));
        self.run(move |conn| ops::list_sources(conn, &ctx, &space, tag.as_deref()))
            .await
    }

    async fn delete_source(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        source_id: &SourceId,
    ) -> Result<bool, MemoryError> {
        let (ctx, space, source_id) = (ctx.clone(), space.clone(), source_id.clone());
        self.run(move |conn| ops::delete_source(conn, &ctx, &space, &source_id))
            .await
    }

    async fn annotation_head(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<RevisionId, MemoryError> {
        let (ctx, space, unit) = (ctx.clone(), space.clone(), unit.clone());
        self.run(move |conn| ops::annotation_head(conn, &ctx, &space, &unit))
            .await
    }

    async fn append_annotation(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        annotation: NewAnnotation,
    ) -> Result<Annotation, MemoryError> {
        let (ctx, space) = (ctx.clone(), space.clone());
        self.run(move |conn| ops::append_annotation(conn, &ctx, &space, &annotation))
            .await
    }

    async fn annotations(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<Vec<Annotation>, MemoryError> {
        let (ctx, space, unit) = (ctx.clone(), space.clone(), unit.clone());
        self.run(move |conn| ops::annotations(conn, &ctx, &space, &unit))
            .await
    }

    async fn register_generation(
        &self,
        spec: EmbeddingSpec,
        segmenter_version: &str,
    ) -> Result<IndexGeneration, MemoryError> {
        let segmenter_version = segmenter_version.to_owned();
        self.run(move |conn| ops::register_generation(conn, &spec, &segmenter_version))
            .await
    }

    async fn set_generation_state(
        &self,
        generation: &GenerationId,
        state: GenerationState,
    ) -> Result<IndexGeneration, MemoryError> {
        let generation = generation.clone();
        self.run(move |conn| ops::set_generation_state(conn, &generation, state))
            .await
    }

    async fn put_embeddings(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        generation: &GenerationId,
        vectors: Vec<(UnitId, Vec<f32>)>,
    ) -> Result<(), MemoryError> {
        let (ctx, space, generation) = (ctx.clone(), space.clone(), generation.clone());
        self.run(move |conn| ops::put_embeddings(conn, &ctx, &space, &generation, &vectors))
            .await
    }
}
