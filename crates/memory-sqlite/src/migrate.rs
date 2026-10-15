// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The versioned schema.
//!
//! The schema version lives in SQLite's `user_version`. A database from before
//! versioning has version 0: its tables belong to the legacy store and are
//! left untouched, because every table of this adapter is prefixed `mem_` and
//! shares no name with them. A database written by a newer build is refused
//! rather than read with the wrong idea of its layout.

use rusqlite::{Connection, TransactionBehavior};
use sven_memory_api::MemoryError;

use crate::storage;

/// One entry per schema version, applied in order. Appending an entry is the
/// only way to change the schema; an applied entry is never edited.
const MIGRATIONS: &[&str] = &[
    // Version 1.
    "CREATE TABLE mem_sources (
         id           INTEGER PRIMARY KEY AUTOINCREMENT,
         space_id     TEXT NOT NULL,
         source_id    TEXT NOT NULL,
         revision     TEXT NOT NULL,
         label        TEXT NOT NULL,
         text         TEXT NOT NULL,
         origin       TEXT NOT NULL,
         captured_at  INTEGER NOT NULL,
         training_use TEXT NOT NULL,
         UNIQUE (space_id, source_id)
     );
     CREATE TABLE mem_source_tags (
         source_row INTEGER NOT NULL REFERENCES mem_sources(id) ON DELETE CASCADE,
         tag        TEXT NOT NULL,
         PRIMARY KEY (source_row, tag)
     );
     CREATE INDEX mem_source_tags_by_tag ON mem_source_tags(tag);
     CREATE TABLE mem_units (
         id            INTEGER PRIMARY KEY,
         source_row    INTEGER NOT NULL REFERENCES mem_sources(id) ON DELETE CASCADE,
         unit_id       TEXT NOT NULL UNIQUE,
         ordinal       INTEGER NOT NULL,
         kind          TEXT NOT NULL,
         heading_path  TEXT NOT NULL,
         block_ordinal INTEGER NOT NULL,
         fingerprint   TEXT NOT NULL,
         byte_start    INTEGER NOT NULL,
         byte_end      INTEGER NOT NULL,
         char_start    INTEGER NOT NULL,
         char_end      INTEGER NOT NULL,
         text          TEXT NOT NULL
     );
     CREATE INDEX mem_units_by_source ON mem_units(source_row);
     CREATE VIRTUAL TABLE mem_units_fts USING fts5(
         text, content='mem_units', content_rowid='id'
     );
     CREATE TRIGGER mem_units_ai AFTER INSERT ON mem_units BEGIN
         INSERT INTO mem_units_fts(rowid, text) VALUES (new.id, new.text);
     END;
     CREATE TRIGGER mem_units_ad AFTER DELETE ON mem_units BEGIN
         INSERT INTO mem_units_fts(mem_units_fts, rowid, text)
             VALUES ('delete', old.id, old.text);
     END;
     CREATE TABLE mem_generations (
         generation_id     TEXT PRIMARY KEY,
         spec              TEXT NOT NULL,
         segmenter_version TEXT NOT NULL,
         state             TEXT NOT NULL
     );
     CREATE TABLE mem_embeddings (
         unit_row      INTEGER NOT NULL REFERENCES mem_units(id) ON DELETE CASCADE,
         generation_id TEXT NOT NULL REFERENCES mem_generations(generation_id),
         vector        BLOB NOT NULL,
         PRIMARY KEY (unit_row, generation_id)
     );
     CREATE TABLE mem_annotations (
         id                 INTEGER PRIMARY KEY AUTOINCREMENT,
         annotation_id      TEXT NOT NULL UNIQUE,
         space_id           TEXT NOT NULL,
         source_id          TEXT NOT NULL,
         unit_id            TEXT NOT NULL,
         author             TEXT NOT NULL,
         content            TEXT NOT NULL,
         resulting_revision TEXT NOT NULL
     );
     CREATE INDEX mem_annotations_by_unit ON mem_annotations(space_id, source_id, unit_id);",
];

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

fn user_version(conn: &Connection) -> Result<i64, MemoryError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)
}

/// Brings `conn` to [`SCHEMA_VERSION`]. Idempotent, and safe against another
/// process migrating the same file at the same moment: each step runs in an
/// immediate transaction that re-reads the version first.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), MemoryError> {
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let target = index as i64 + 1;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = user_version(&tx)?;
        if current > SCHEMA_VERSION {
            return Err(MemoryError::Unavailable(format!(
                "database schema version {current} is newer than this build supports \
                 ({SCHEMA_VERSION})"
            )));
        }
        if current >= target {
            continue;
        }
        tx.execute_batch(sql).map_err(storage)?;
        tx.pragma_update(None, "user_version", target)
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
    }
    Ok(())
}
