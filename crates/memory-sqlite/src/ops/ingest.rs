// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Recording and deleting sources.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sven_memory_api::{
    AccessContext, IngestOutcome, IngestRequest, MemoryError, Permission, RevisionId, SourceId,
    SpaceId, UnitId,
};

use crate::{rows::enum_name, storage};

pub(crate) fn ingest(
    conn: &mut Connection,
    ctx: &AccessContext,
    request: &IngestRequest,
) -> Result<IngestOutcome, MemoryError> {
    ctx.require(&request.space, Permission::Ingest)?;
    request.validate()?;

    let revision = request.revision();
    let units: Vec<UnitId> = (0..request.units.len())
        .map(|ordinal| {
            UnitId::derive(
                &request.space,
                &request.source_id,
                &revision,
                ordinal as u32,
            )
        })
        .collect();

    let tx = conn.transaction().map_err(storage)?;
    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, revision FROM mem_sources WHERE space_id = ?1 AND source_id = ?2",
            params![request.space.as_str(), request.source_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;

    let outcome = match existing {
        Some((row, previous)) if previous == revision.as_str() => {
            // The content is already indexed; only the descriptive fields may
            // have moved, including the training-use label.
            write_descriptors(&tx, row, request)?;
            IngestOutcome::Unchanged { revision, units }
        }
        Some((row, previous)) => {
            tx.execute("DELETE FROM mem_units WHERE source_row = ?1", [row])
                .map_err(storage)?;
            tx.execute(
                "UPDATE mem_sources SET revision = ?2, text = ?3 WHERE id = ?1",
                params![row, revision.as_str(), request.text],
            )
            .map_err(storage)?;
            write_descriptors(&tx, row, request)?;
            insert_units(&tx, row, request, &units)?;
            IngestOutcome::Replaced {
                revision,
                previous: RevisionId::new(previous),
                units,
            }
        }
        None => {
            let d = Descriptors::of(request)?;
            tx.execute(
                "INSERT INTO mem_sources
                     (space_id, source_id, revision, label, text, origin, captured_at, training_use)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    request.space.as_str(),
                    request.source_id.as_str(),
                    revision.as_str(),
                    request.label,
                    request.text,
                    d.origin,
                    d.captured_at,
                    d.training_use
                ],
            )
            .map_err(storage)?;
            let row = tx.last_insert_rowid();
            write_tags(&tx, row, request)?;
            insert_units(&tx, row, request, &units)?;
            IngestOutcome::Created { revision, units }
        }
    };
    tx.commit().map_err(storage)?;
    Ok(outcome)
}

/// The descriptive columns of a source row, in their stored form.
struct Descriptors {
    origin: String,
    captured_at: i64,
    training_use: String,
}

impl Descriptors {
    fn of(request: &IngestRequest) -> Result<Self, MemoryError> {
        Ok(Self {
            origin: serde_json::to_string(&request.provenance.origin)
                .map_err(|e| MemoryError::Storage(e.to_string()))?,
            captured_at: i64::try_from(request.provenance.captured_at)
                .map_err(|_| MemoryError::InvalidInput("capture time out of range".into()))?,
            training_use: enum_name(&request.provenance.training_use)?,
        })
    }
}

fn write_descriptors(
    tx: &Transaction<'_>,
    row: i64,
    request: &IngestRequest,
) -> Result<(), MemoryError> {
    let d = Descriptors::of(request)?;
    tx.execute(
        "UPDATE mem_sources
            SET label = ?2, origin = ?3, captured_at = ?4, training_use = ?5
          WHERE id = ?1",
        params![row, request.label, d.origin, d.captured_at, d.training_use],
    )
    .map_err(storage)?;
    tx.execute("DELETE FROM mem_source_tags WHERE source_row = ?1", [row])
        .map_err(storage)?;
    write_tags(tx, row, request)
}

fn write_tags(tx: &Transaction<'_>, row: i64, request: &IngestRequest) -> Result<(), MemoryError> {
    for tag in &request.tags {
        tx.execute(
            "INSERT INTO mem_source_tags (source_row, tag) VALUES (?1, ?2)",
            params![row, tag],
        )
        .map_err(storage)?;
    }
    Ok(())
}

fn insert_units(
    tx: &Transaction<'_>,
    row: i64,
    request: &IngestRequest,
    ids: &[UnitId],
) -> Result<(), MemoryError> {
    let mut insert = tx
        .prepare(
            "INSERT INTO mem_units
                 (source_row, unit_id, ordinal, kind, heading_path, block_ordinal, fingerprint,
                  byte_start, byte_end, char_start, char_end, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .map_err(storage)?;
    for (ordinal, (unit, id)) in request.units.iter().zip(ids).enumerate() {
        let heading_path = serde_json::to_string(&unit.anchor.heading_path)
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
        insert
            .execute(params![
                row,
                id.as_str(),
                ordinal as i64,
                enum_name(&unit.kind)?,
                heading_path,
                unit.anchor.block_ordinal,
                unit.anchor.text_fingerprint,
                unit.byte_range.start as i64,
                unit.byte_range.end as i64,
                unit.char_range.start as i64,
                unit.char_range.end as i64,
                unit.text,
            ])
            .map_err(storage)?;
    }
    Ok(())
}

pub(crate) fn delete_source(
    conn: &mut Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    source: &SourceId,
) -> Result<bool, MemoryError> {
    ctx.require(space, Permission::Delete)?;
    let tx = conn.transaction().map_err(storage)?;
    let row: Option<i64> = tx
        .query_row(
            "SELECT id FROM mem_sources WHERE space_id = ?1 AND source_id = ?2",
            params![space.as_str(), source.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let Some(row) = row else {
        return Ok(false);
    };
    // Units go first and explicitly so the full-text index rows are removed by
    // their trigger; embeddings and tags follow by foreign key.
    tx.execute("DELETE FROM mem_units WHERE source_row = ?1", [row])
        .map_err(storage)?;
    tx.execute(
        "DELETE FROM mem_annotations WHERE space_id = ?1 AND source_id = ?2",
        params![space.as_str(), source.as_str()],
    )
    .map_err(storage)?;
    tx.execute("DELETE FROM mem_sources WHERE id = ?1", [row])
        .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(true)
}
