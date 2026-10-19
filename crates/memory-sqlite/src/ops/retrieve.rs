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
    if limit == 0 || spaces.is_empty() {
        return Ok(empty);
    }
    let space_list = serde_json::to_string(&spaces.iter().map(SpaceId::as_str).collect::<Vec<_>>())
        .map_err(|e| MemoryError::Storage(e.to_string()))?;

    let Some(query_vector) = &request.query_embedding else {
        let hits = lexical(conn, &space_list, request, limit)?;
        return Ok(RetrievalResponse {
            hits: ranked(hits)?,
            ..empty
        });
    };

    // Hybrid: the lexical and the vector ranking, each cut to a wider
    // candidate list than the answer, are fused by reciprocal rank.
    let generation = request
        .generation
        .as_ref()
        .expect("validate() refuses a query vector without a generation");
    let dims = generation_dims(conn, generation)?;
    if query_vector.len() != dims {
        return Err(MemoryError::InvalidInput(format!(
            "the query embedding has {} values, the generation expects {dims}",
            query_vector.len()
        )));
    }
    let candidates = (limit * CANDIDATE_FACTOR).max(MIN_CANDIDATES);
    let by_word = lexical(conn, &space_list, request, candidates)?;
    let by_meaning = nearest(
        conn,
        &space_list,
        request,
        generation,
        query_vector,
        candidates,
    )?;
    let hits = fuse(by_word, by_meaning, limit);
    Ok(RetrievalResponse {
        hits: ranked(hits)?,
        ..empty
    })
}

/// How many more candidates than answers each ranking contributes to a fusion.
const CANDIDATE_FACTOR: usize = 4;
/// The fewest candidates each ranking contributes.
const MIN_CANDIDATES: usize = 20;
/// The constant of reciprocal rank fusion (Cormack, Clarke and Buettcher,
/// 2009): large enough that no single ranking's first place dominates.
const RRF_K: f64 = 60.0;

/// One unit found, before the final order is fixed.
struct Found {
    unit: UnitRow,
    space: String,
    provenance: ProvenanceRow,
    score: f64,
}

fn generation_dims(
    conn: &Connection,
    generation: &sven_memory_api::GenerationId,
) -> Result<usize, MemoryError> {
    super::generations::load(conn, generation)?
        .map(|g| g.spec.dims)
        .ok_or_else(|| MemoryError::GenerationRefused {
            generation: generation.clone(),
            reason: "the generation is not registered in this store".into(),
        })
}

fn ranked(found: Vec<Found>) -> Result<Vec<RetrievalHit>, MemoryError> {
    let mut hits = Vec::with_capacity(found.len());
    for (index, f) in found.into_iter().enumerate() {
        hits.push(RetrievalHit {
            space: SpaceId::new(f.space),
            unit: f.unit.into_unit()?,
            provenance: f.provenance.into_provenance()?,
            score: f.score as f32,
            rank: index + 1,
        });
    }
    Ok(hits)
}

/// Reciprocal rank fusion of two rankings: a unit scores the sum of
/// `1 / (RRF_K + rank)` over the rankings that hold it. Ties break on the
/// unit id, so the order is the same every time.
fn fuse(first: Vec<Found>, second: Vec<Found>, limit: usize) -> Vec<Found> {
    let mut fused: std::collections::BTreeMap<String, Found> = std::collections::BTreeMap::new();
    for ranking in [first, second] {
        for (index, mut f) in ranking.into_iter().enumerate() {
            let share = 1.0 / (RRF_K + (index + 1) as f64);
            let id = f.unit.unit_id().to_owned();
            match fused.get_mut(&id) {
                Some(seen) => seen.score += share,
                None => {
                    f.score = share;
                    fused.insert(id, f);
                }
            }
        }
    }
    let mut all: Vec<Found> = fused.into_values().collect();
    all.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.unit.unit_id().cmp(b.unit.unit_id()))
    });
    all.truncate(limit);
    all
}

fn found_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<(UnitRow, String, ProvenanceRow)> {
    Ok((
        UnitRow::read(row)?,
        row.get::<_, String>(12)?,
        ProvenanceRow {
            origin: row.get(13)?,
            captured_at: row.get(14)?,
            training_use: row.get(15)?,
        },
    ))
}

fn lexical(
    conn: &Connection,
    space_list: &str,
    request: &RetrievalRequest,
    limit: usize,
) -> Result<Vec<Found>, MemoryError> {
    let Some(expression) = fts_expression(&request.query) else {
        return Ok(Vec::new());
    };
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
    let mut statement = conn.prepare(&sql).map_err(storage)?;
    let raw = statement
        .query_map(
            params![expression, space_list, request.tag, limit as i64],
            |row| {
                let (unit, space, provenance) = found_columns(row)?;
                Ok(Found {
                    unit,
                    space,
                    provenance,
                    score: row.get::<_, f64>(16)?,
                })
            },
        )
        .map_err(storage)?;
    collect_rows(raw)
}

/// The units of the caller's spaces nearest to `query` by cosine similarity
/// under `generation`, nearest first, ties by unit id. A unit without a vector
/// under the generation is not in this ranking.
fn nearest(
    conn: &Connection,
    space_list: &str,
    request: &RetrievalRequest,
    generation: &sven_memory_api::GenerationId,
    query: &[f32],
    limit: usize,
) -> Result<Vec<Found>, MemoryError> {
    let sql = format!(
        "SELECT {UNIT_COLUMNS}, s.space_id, s.origin, s.captured_at, s.training_use, e.vector
           FROM mem_embeddings e
           JOIN mem_units u ON u.id = e.unit_row
           JOIN mem_sources s ON s.id = u.source_row
          WHERE e.generation_id = ?1
            AND s.space_id IN (SELECT value FROM json_each(?2))
            AND (?3 IS NULL OR EXISTS (
                   SELECT 1 FROM mem_source_tags t WHERE t.source_row = s.id AND t.tag = ?3))"
    );
    let mut statement = conn.prepare(&sql).map_err(storage)?;
    let raw = statement
        .query_map(
            params![generation.as_str(), space_list, request.tag],
            |row| {
                let (unit, space, provenance) = found_columns(row)?;
                Ok((unit, space, provenance, row.get::<_, Vec<u8>>(16)?))
            },
        )
        .map_err(storage)?;
    let query_norm = norm(query);
    let mut found = Vec::new();
    for (unit, space, provenance, blob) in collect_rows(raw)? {
        let (chunks, rest) = blob.as_chunks::<4>();
        if !rest.is_empty() {
            return Err(MemoryError::Corrupt(
                "a stored embedding is not a whole number of values".into(),
            ));
        }
        let vector: Vec<f32> = chunks.iter().map(|b| f32::from_le_bytes(*b)).collect();
        if vector.len() != query.len() {
            return Err(MemoryError::Corrupt(
                "a stored embedding does not fit its generation".into(),
            ));
        }
        let denominator = query_norm * norm(&vector);
        let score = if denominator > 0.0 {
            f64::from(vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>())
                / f64::from(denominator)
        } else {
            0.0
        };
        found.push(Found {
            unit,
            space,
            provenance,
            score,
        });
    }
    found.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.unit.unit_id().cmp(b.unit.unit_id()))
    });
    found.truncate(limit);
    Ok(found)
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
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
