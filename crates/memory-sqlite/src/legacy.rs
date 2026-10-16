// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Moving the rows of the store that predates this adapter into a space.
//!
//! The previous store kept one flat `memories` table in the same database
//! file. Its rows become notes of a personal space, then the table is removed,
//! all in one immediate transaction: either every importable row has moved and
//! the old table is gone, or nothing changed. A crash, an error or a second
//! process opening the same file at the same moment can therefore neither lose
//! a row nor record one twice, and a note forgotten after the move cannot come
//! back, because the table that could re-import it no longer exists.
//!
//! A row the contract cannot hold (more tags than a source may carry) stays in
//! the old table and is reported; the table is dropped only once it is empty.
//! Embeddings are not carried over: the previous store never computed one, and
//! a vector is only comparable within an index generation.
//!
//! Swedish Embedded AB implements solutions for moving agent memory between
//! storage generations without losing a record for its clients. If your team
//! needs expertise in migrating an agent's notes safely then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::collections::HashMap;

use rusqlite::{Connection, Transaction, TransactionBehavior};
use sven_memory_api::{
    sha256_hex, AccessContext, IngestRequest, MemoryError, Note, Origin, Permission, Provenance,
    SourceId, SpaceId, TrainingUse,
};

use crate::{ops, storage, SqliteMemory};

/// The identity recorded for the import.
const IMPORTER: &str = "legacy-import";
/// Tag of a note whose stored metadata could not be read.
const UNREADABLE_METADATA_TAG: &str = "legacy-metadata-unreadable";
/// Metadata key a writer stamped to confine a row to one session.
const SESSION_SCOPE_KEY: &str = "session_scope";
/// Metadata key a writer stamped with the provenance it resolved.
const PROVENANCE_KEY: &str = "provenance";
/// The provenance label of text a person stated.
const USER_STATED: &str = "user_stated";

/// What an import did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegacyImport {
    /// Rows moved into notes.
    pub imported: usize,
    /// Rows the contract cannot hold, left in the old table.
    pub kept: usize,
}

impl SqliteMemory {
    /// Moves the rows of the previous store into `personal`, once.
    ///
    /// A row confined to a session goes to a space of its own that no
    /// ordinary context reads, so the confinement survives the move. Rows an
    /// agent wrote are imported as untrusted material; only a row stamped as
    /// stated by a person is trusted.
    ///
    /// # Errors
    /// [`MemoryError::Storage`] if the database fails; the transaction is
    /// then rolled back and the old rows are intact.
    pub async fn import_legacy(&self, personal: &SpaceId) -> Result<LegacyImport, MemoryError> {
        let personal = personal.clone();
        self.run(move |conn| import(conn, &personal)).await
    }
}

struct LegacyRow {
    id: i64,
    content: String,
    metadata: String,
    captured_at: i64,
}

impl LegacyRow {
    /// The request that records this row as a note, and the space it goes to.
    fn request(&self, personal: &SpaceId) -> (SpaceId, IngestRequest) {
        let readable: Option<HashMap<String, String>> = serde_json::from_str(&self.metadata).ok();
        let metadata = readable.clone().unwrap_or_default();
        let space = match metadata.get(SESSION_SCOPE_KEY) {
            Some(scope) => SpaceId::new(format!("legacy-session-{scope}")),
            None => personal.clone(),
        };
        let mut tags: Vec<String> = metadata
            .get("tags")
            .map(|joined| joined.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        tags.retain(|tag| !tag.is_empty());
        if readable.is_none() {
            tags.push(UNREADABLE_METADATA_TAG.to_owned());
        }
        let origin = if metadata.get(PROVENANCE_KEY).map(String::as_str) == Some(USER_STATED) {
            Origin::User
        } else {
            Origin::Import {
                format: "semantic-memory".to_owned(),
            }
        };
        let note = Note {
            space: space.clone(),
            source_id: SourceId::new(format!(
                "n-{}",
                &sha256_hex(format!("legacy-memory-row:{}", self.id).as_bytes())[..16]
            )),
            text: &self.content,
            entity: metadata.get("entity").map(String::as_str),
            source: metadata.get("source").map(String::as_str),
            tags,
            provenance: Provenance {
                origin,
                captured_at: u64::try_from(self.captured_at).unwrap_or(0),
                training_use: TrainingUse::Unspecified,
            },
        };
        (space, note.into_request())
    }
}

fn import(conn: &mut Connection, personal: &SpaceId) -> Result<LegacyImport, MemoryError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    if !has_table(&tx, "memories")? {
        return Ok(LegacyImport::default());
    }
    let mut moved = Vec::new();
    let mut kept = 0;
    for row in read_rows(&tx)? {
        let (space, request) = row.request(personal);
        let ctx = AccessContext::new(IMPORTER).writing(space, &[Permission::Ingest]);
        match ops::ingest_in(&tx, &ctx, &request) {
            Ok(_) => moved.push(row.id),
            Err(MemoryError::InvalidInput(reason)) => {
                tracing::warn!(%reason, "a row of the previous memory store stays where it is");
                kept += 1;
            }
            Err(other) => return Err(other),
        }
    }
    if kept == 0 {
        tx.execute_batch("DROP TABLE IF EXISTS memories_fts; DROP TABLE memories;")
            .map_err(storage)?;
    } else {
        for id in &moved {
            tx.execute("DELETE FROM memories WHERE id = ?1", [id])
                .map_err(storage)?;
        }
    }
    tx.commit().map_err(storage)?;
    Ok(LegacyImport {
        imported: moved.len(),
        kept,
    })
}

fn has_table(tx: &Transaction<'_>, name: &str) -> Result<bool, MemoryError> {
    tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(storage)
}

fn read_rows(tx: &Transaction<'_>) -> Result<Vec<LegacyRow>, MemoryError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, content, metadata, COALESCE(CAST(strftime('%s', created) AS INTEGER), 0)
               FROM memories ORDER BY id",
        )
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(LegacyRow {
                id: row.get(0)?,
                content: row.get(1)?,
                metadata: row.get(2)?,
                captured_at: row.get(3)?,
            })
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    Ok(rows)
}
