// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Index generations and the embeddings stored under them.

use rusqlite::{params, Connection, OptionalExtension};
use sven_memory_api::{
    AccessContext, EmbeddingSpec, GenerationId, GenerationState, IndexGeneration, MemoryError,
    Permission, SpaceId, UnitId,
};

use crate::{
    rows::{enum_name, from_json},
    storage,
};

fn state_from(name: &str) -> Result<GenerationState, MemoryError> {
    from_json(&format!("\"{name}\""), "generation state")
}

/// The state of `generation`, or `None` if this store has never heard of it.
pub(super) fn generation_state(
    conn: &Connection,
    generation: &GenerationId,
) -> Result<Option<GenerationState>, MemoryError> {
    conn.query_row(
        "SELECT state FROM mem_generations WHERE generation_id = ?1",
        [generation.as_str()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(storage)?
    .map(|name| state_from(&name))
    .transpose()
}

pub(super) fn load(
    conn: &Connection,
    generation: &GenerationId,
) -> Result<Option<IndexGeneration>, MemoryError> {
    let row = conn
        .query_row(
            "SELECT spec, segmenter_version, state FROM mem_generations WHERE generation_id = ?1",
            [generation.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    row.map(|(spec, segmenter_version, state)| {
        Ok(IndexGeneration {
            generation_id: generation.clone(),
            spec: from_json(&spec, "embedding spec")?,
            segmenter_version,
            state: state_from(&state)?,
        })
    })
    .transpose()
}

fn refused(generation: &GenerationId, reason: &str) -> MemoryError {
    MemoryError::GenerationRefused {
        generation: generation.clone(),
        reason: reason.to_owned(),
    }
}

pub(crate) fn register_generation(
    conn: &Connection,
    spec: &EmbeddingSpec,
    segmenter_version: &str,
) -> Result<IndexGeneration, MemoryError> {
    let id = spec.generation_id(segmenter_version);
    let json = serde_json::to_string(spec).map_err(|e| MemoryError::Storage(e.to_string()))?;
    conn.execute(
        "INSERT OR IGNORE INTO mem_generations (generation_id, spec, segmenter_version, state)
         VALUES (?1, ?2, ?3, 'building')",
        params![id.as_str(), json, segmenter_version],
    )
    .map_err(storage)?;
    load(conn, &id)?.ok_or_else(|| MemoryError::Storage("registered generation vanished".into()))
}

pub(crate) fn set_generation_state(
    conn: &Connection,
    generation: &GenerationId,
    state: GenerationState,
) -> Result<IndexGeneration, MemoryError> {
    let current = load(conn, generation)?
        .ok_or_else(|| refused(generation, "the generation is not registered in this store"))?;
    if current.state == GenerationState::Retired && state != GenerationState::Retired {
        return Err(refused(
            generation,
            "a retired generation cannot be revived",
        ));
    }
    conn.execute(
        "UPDATE mem_generations SET state = ?2 WHERE generation_id = ?1",
        params![generation.as_str(), enum_name(&state)?],
    )
    .map_err(storage)?;
    Ok(IndexGeneration { state, ..current })
}

pub(crate) fn put_embeddings(
    conn: &mut Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    generation: &GenerationId,
    vectors: &[(UnitId, Vec<f32>)],
) -> Result<(), MemoryError> {
    ctx.require(space, Permission::Ingest)?;
    let spec = load(conn, generation)?
        .filter(|g| g.state != GenerationState::Retired)
        .ok_or_else(|| refused(generation, "the generation is unknown or retired"))?
        .spec;
    for (unit, vector) in vectors {
        if vector.len() != spec.dims {
            return Err(MemoryError::InvalidInput(format!(
                "embedding of unit {unit} has {} values, the generation expects {}",
                vector.len(),
                spec.dims
            )));
        }
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(MemoryError::InvalidInput(format!(
                "embedding of unit {unit} holds a non-finite value"
            )));
        }
    }

    let tx = conn.transaction().map_err(storage)?;
    for (unit, vector) in vectors {
        let row: Option<i64> = tx
            .query_row(
                "SELECT u.id FROM mem_units u JOIN mem_sources s ON s.id = u.source_row
                  WHERE s.space_id = ?1 AND u.unit_id = ?2",
                params![space.as_str(), unit.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        let row = row.ok_or(MemoryError::NotFound)?;
        let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        tx.execute(
            "INSERT OR REPLACE INTO mem_embeddings (unit_row, generation_id, vector)
             VALUES (?1, ?2, ?3)",
            params![row, generation.as_str(), blob],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}
