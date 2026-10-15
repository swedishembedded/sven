// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading: lexical retrieval, lookup by id and listing.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension};
use sven_memory_api::{
    AccessContext, GenerationState, MemoryError, RetrievalHit, RetrievalRequest, RetrievalResponse,
    RevisionId, SourceId, SourceSummary, SpaceId, Unit, UnitId,
};

use super::generations::generation_state;
use crate::{
    rows::{collect_rows, ProvenanceRow, UnitRow, UNIT_COLUMNS},
    storage,
};

/// Most search terms one query turns into. Further words are ignored.
const MAX_QUERY_TERMS: usize = 64;

/// Turns free text into an FTS5 expression: every alphanumeric run becomes a
/// quoted prefix term and the terms are alternatives. Nothing the caller types
/// can reach FTS5's operator syntax, because only alphanumerics survive and
/// each term is quoted. `None` if the text holds no term.
fn fts_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(MAX_QUERY_TERMS)
        .map(|term| format!("\"{term}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

pub(crate) fn retrieve(
    conn: &Connection,
    ctx: &AccessContext,
    request: &RetrievalRequest,
) -> Result<RetrievalResponse, MemoryError> {
    request.validate()?;
    let spaces: BTreeSet<SpaceId> = match &request.spaces {
        None => ctx.read.clone(),
        Some(wanted) => {
            if let Some(hidden) = wanted.iter().find(|s| !ctx.can_read(s)) {
                tracing::debug!(space = %hidden, "retrieval named a space the caller cannot read");
                return Err(MemoryError::NotFound);
            }
            wanted.clone()
        }
    };
    if let Some(generation) = &request.generation {
        match generation_state(conn, generation)? {
            Some(GenerationState::Active) => {}
            Some(state) => {
                return Err(MemoryError::GenerationRefused {
                    generation: generation.clone(),
                    reason: format!("the generation is {state:?}, not Active"),
                });
            }
            None => {
                return Err(MemoryError::GenerationRefused {
                    generation: generation.clone(),
                    reason: "the generation is not registered in this store".into(),
                });
            }
        }
    }
    let empty = RetrievalResponse {
        hits: Vec::new(),
        spaces: spaces.clone(),
        generation: request.generation.clone(),
    };
    let limit = request.limit.min(ctx.budget.max_results);
    let (Some(expression), true) = (fts_expression(&request.query), limit > 0) else {
        return Ok(empty);
    };
    if spaces.is_empty() {
        return Ok(empty);
    }

    // The scope and the tag are conditions of the query itself, so the limit
    // applies to rows the caller may see.
    let sql = format!(
        "SELECT {UNIT_COLUMNS}, s.space_id, s.origin, s.captured_at, s.training_use,
                -bm25(mem_units_fts) AS score
           FROM mem_units_fts
           JOIN mem_units u ON u.id = mem_units_fts.rowid
           JOIN mem_sources s ON s.id = u.source_row
          WHERE mem_units_fts MATCH ?1
            AND s.space_id IN (SELECT value FROM json_each(?2))
            AND (?3 IS NULL OR EXISTS (
                   SELECT 1 FROM mem_source_tags t WHERE t.source_row = s.id AND t.tag = ?3))
          ORDER BY score DESC, u.unit_id
          LIMIT ?4"
    );
    let space_list = serde_json::to_string(&spaces.iter().map(SpaceId::as_str).collect::<Vec<_>>())
        .map_err(|e| MemoryError::Storage(e.to_string()))?;
    let mut statement = conn.prepare(&sql).map_err(storage)?;
    let raw = statement
        .query_map(
            params![expression, space_list, request.tag, limit as i64],
            |row| {
                Ok((
                    UnitRow::read(row)?,
                    row.get::<_, String>(12)?,
                    ProvenanceRow {
                        origin: row.get(13)?,
                        captured_at: row.get(14)?,
                        training_use: row.get(15)?,
                    },
                    row.get::<_, f64>(16)?,
                ))
            },
        )
        .map_err(storage)?;
    let mut hits = Vec::new();
    for (index, row) in collect_rows(raw)?.into_iter().enumerate() {
        let (unit, space, provenance, score) = row;
        hits.push(RetrievalHit {
            space: SpaceId::new(space),
            unit: unit.into_unit()?,
            provenance: provenance.into_provenance()?,
            score: score as f32,
            rank: index + 1,
        });
    }
    Ok(RetrievalResponse { hits, ..empty })
}

pub(crate) fn get_unit(
    conn: &Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    unit_id: &UnitId,
) -> Result<Option<Unit>, MemoryError> {
    if !ctx.can_read(space) {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {UNIT_COLUMNS} FROM mem_units u JOIN mem_sources s ON s.id = u.source_row
          WHERE s.space_id = ?1 AND u.unit_id = ?2"
    );
    conn.query_row(
        &sql,
        params![space.as_str(), unit_id.as_str()],
        UnitRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(UnitRow::into_unit)
    .transpose()
}

pub(crate) fn list_sources(
    conn: &Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    tag: Option<&str>,
) -> Result<Vec<SourceSummary>, MemoryError> {
    if !ctx.can_read(space) {
        return Err(MemoryError::NotFound);
    }
    let mut statement = conn
        .prepare(
            "SELECT s.id, s.source_id, s.label, s.revision, s.origin, s.captured_at,
                    s.training_use, (SELECT COUNT(*) FROM mem_units u WHERE u.source_row = s.id)
               FROM mem_sources s
              WHERE s.space_id = ?1
                AND (?2 IS NULL OR EXISTS (
                       SELECT 1 FROM mem_source_tags t WHERE t.source_row = s.id AND t.tag = ?2))
              ORDER BY s.id",
        )
        .map_err(storage)?;
    let raw = statement
        .query_map(params![space.as_str(), tag], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                ProvenanceRow {
                    origin: row.get(4)?,
                    captured_at: row.get(5)?,
                    training_use: row.get(6)?,
                },
                row.get::<_, i64>(7)?,
            ))
        })
        .map_err(storage)?;
    let mut sources = Vec::new();
    for (row, source_id, label, revision, provenance, unit_count) in collect_rows(raw)? {
        sources.push(SourceSummary {
            source_id: SourceId::new(source_id),
            label,
            revision: RevisionId::new(revision),
            tags: tags_of(conn, row)?,
            provenance: provenance.into_provenance()?,
            unit_count: usize::try_from(unit_count)
                .map_err(|_| MemoryError::Corrupt("unit count out of range".into()))?,
        });
    }
    Ok(sources)
}

fn tags_of(conn: &Connection, source_row: i64) -> Result<BTreeSet<String>, MemoryError> {
    let mut statement = conn
        .prepare("SELECT tag FROM mem_source_tags WHERE source_row = ?1")
        .map_err(storage)?;
    let tags = statement
        .query_map([source_row], |row| row.get::<_, String>(0))
        .map_err(storage)?;
    Ok(collect_rows(tags)?.into_iter().collect())
}
