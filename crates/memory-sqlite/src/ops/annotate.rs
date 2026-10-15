// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The annotation log.
//!
//! Every unit has a log revision: its source's revision until the first
//! annotation, then a digest chained over each annotation appended. A writer
//! states the revision it read, so two writers who read the same one cannot
//! both append, and neither can a writer who read before the source changed.

use rusqlite::{params, Connection, OptionalExtension};
use sven_memory_api::{
    sha256_hex, AccessContext, Annotation, AnnotationId, MemoryError, NewAnnotation, Permission,
    RevisionId, SpaceId, UnitAnchor,
};

use crate::{
    rows::{collect_rows, from_json},
    storage,
};

/// Longest annotation body, in bytes.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// The log revision of `target`, or why it has none.
fn head_of(
    conn: &Connection,
    space: &SpaceId,
    target: &UnitAnchor,
) -> Result<RevisionId, MemoryError> {
    let source: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, revision FROM mem_sources WHERE space_id = ?1 AND source_id = ?2",
            params![space.as_str(), target.source_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let (source_row, source_revision) = source.ok_or(MemoryError::NotFound)?;

    let unit_exists: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM mem_units WHERE source_row = ?1 AND unit_id = ?2)",
            params![source_row, target.unit_id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if !unit_exists {
        // A unit of an older revision is stale, not missing: say where the
        // writer should re-anchor.
        return if source_revision != target.revision.as_str() {
            Err(MemoryError::Conflict {
                current: RevisionId::new(source_revision),
            })
        } else {
            Err(MemoryError::NotFound)
        };
    }

    let last: Option<String> = conn
        .query_row(
            "SELECT resulting_revision FROM mem_annotations
              WHERE space_id = ?1 AND source_id = ?2 AND unit_id = ?3
              ORDER BY id DESC LIMIT 1",
            params![
                space.as_str(),
                target.source_id.as_str(),
                target.unit_id.as_str()
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(RevisionId::new(last.unwrap_or(source_revision)))
}

pub(crate) fn annotation_head(
    conn: &Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    target: &UnitAnchor,
) -> Result<RevisionId, MemoryError> {
    if !ctx.can_read(space) {
        return Err(MemoryError::NotFound);
    }
    head_of(conn, space, target)
}

pub(crate) fn append_annotation(
    conn: &mut Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    annotation: &NewAnnotation,
) -> Result<Annotation, MemoryError> {
    ctx.require(space, Permission::Annotate)?;
    if annotation.body.is_empty() || annotation.body.len() > MAX_BODY_BYTES {
        return Err(MemoryError::InvalidInput(format!(
            "annotation body must be 1..={MAX_BODY_BYTES} bytes"
        )));
    }

    // One immediate transaction covers the head check and the insert, so two
    // processes cannot both append against the same head.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(storage)?;
    let head = head_of(&tx, space, &annotation.target)?;
    if head != annotation.expected_revision {
        return Err(MemoryError::Conflict { current: head });
    }
    for evidence in &annotation.evidence {
        let unit = crate::ops::get_unit(&tx, ctx, space, &evidence.unit.unit_id)?
            .ok_or_else(|| MemoryError::InvalidInput("evidence names an unknown unit".into()))?;
        if evidence.span.is_empty() || !unit.text.contains(&evidence.span) {
            return Err(MemoryError::InvalidInput(
                "evidence span is not a verbatim passage of its unit".into(),
            ));
        }
    }

    let annotation_id = AnnotationId::new(sha256_hex(
        format!("annotation\0{head}\0{}\0{}", ctx.principal, annotation.body).as_bytes(),
    ));
    let resulting_revision = RevisionId::new(sha256_hex(
        format!("revision\0{head}\0{annotation_id}").as_bytes(),
    ));
    let content =
        serde_json::to_string(annotation).map_err(|e| MemoryError::Storage(e.to_string()))?;
    tx.execute(
        "INSERT INTO mem_annotations
             (annotation_id, space_id, source_id, unit_id, author, content, resulting_revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            annotation_id.as_str(),
            space.as_str(),
            annotation.target.source_id.as_str(),
            annotation.target.unit_id.as_str(),
            ctx.principal,
            content,
            resulting_revision.as_str()
        ],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(Annotation {
        annotation_id,
        author: ctx.principal.clone(),
        content: annotation.clone(),
        resulting_revision,
    })
}

pub(crate) fn annotations(
    conn: &Connection,
    ctx: &AccessContext,
    space: &SpaceId,
    target: &UnitAnchor,
) -> Result<Vec<Annotation>, MemoryError> {
    if !ctx.can_read(space) {
        return Err(MemoryError::NotFound);
    }
    let mut statement = conn
        .prepare(
            "SELECT annotation_id, author, content, resulting_revision FROM mem_annotations
              WHERE space_id = ?1 AND source_id = ?2 AND unit_id = ?3 ORDER BY id",
        )
        .map_err(storage)?;
    let raw = statement
        .query_map(
            params![
                space.as_str(),
                target.source_id.as_str(),
                target.unit_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(storage)?;
    collect_rows(raw)?
        .into_iter()
        .map(|(id, author, content, resulting)| {
            Ok(Annotation {
                annotation_id: AnnotationId::new(id),
                author,
                content: from_json(&content, "annotation")?,
                resulting_revision: RevisionId::new(resulting),
            })
        })
        .collect()
}
