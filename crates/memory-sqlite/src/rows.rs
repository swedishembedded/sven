// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Decoding of stored rows into contract types.
//!
//! A row that cannot be decoded is [`MemoryError::Corrupt`]. It is never
//! skipped and never replaced by a default, so damaged data cannot pass for
//! absent data.

use rusqlite::Row;
use sven_memory_api::{
    Anchor, MemoryError, Origin, Provenance, RevisionId, SourceId, Span, TrainingUse, Unit, UnitId,
};

use crate::storage;

/// Columns selected by [`UNIT_COLUMNS`], undecoded.
pub(crate) struct UnitRow {
    unit_id: String,
    source_id: String,
    revision: String,
    kind: String,
    heading_path: String,
    block_ordinal: i64,
    fingerprint: String,
    byte_start: i64,
    byte_end: i64,
    char_start: i64,
    char_end: i64,
    text: String,
}

/// The columns [`UnitRow::read`] expects, for a query joining `mem_units u`
/// to `mem_sources s`.
pub(crate) const UNIT_COLUMNS: &str = "u.unit_id, s.source_id, s.revision, u.kind, \
     u.heading_path, u.block_ordinal, u.fingerprint, u.byte_start, u.byte_end, \
     u.char_start, u.char_end, u.text";

impl UnitRow {
    pub(crate) fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            unit_id: row.get(0)?,
            source_id: row.get(1)?,
            revision: row.get(2)?,
            kind: row.get(3)?,
            heading_path: row.get(4)?,
            block_ordinal: row.get(5)?,
            fingerprint: row.get(6)?,
            byte_start: row.get(7)?,
            byte_end: row.get(8)?,
            char_start: row.get(9)?,
            char_end: row.get(10)?,
            text: row.get(11)?,
        })
    }

    pub(crate) fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub(crate) fn into_unit(self) -> Result<Unit, MemoryError> {
        Ok(Unit {
            unit_id: UnitId::new(self.unit_id),
            source_id: SourceId::new(self.source_id),
            revision: RevisionId::new(self.revision),
            kind: from_json(&format!("\"{}\"", self.kind), "unit kind")?,
            anchor: Anchor {
                heading_path: from_json(&self.heading_path, "heading path")?,
                block_ordinal: u32::try_from(self.block_ordinal)
                    .map_err(|_| corrupt("block ordinal out of range"))?,
                text_fingerprint: self.fingerprint,
            },
            byte_range: span(self.byte_start, self.byte_end)?,
            char_range: span(self.char_start, self.char_end)?,
            text: self.text,
        })
    }
}

/// The provenance columns of a source row, undecoded.
pub(crate) struct ProvenanceRow {
    pub(crate) origin: String,
    pub(crate) captured_at: i64,
    pub(crate) training_use: String,
}

impl ProvenanceRow {
    pub(crate) fn into_provenance(self) -> Result<Provenance, MemoryError> {
        Ok(Provenance {
            origin: from_json::<Origin>(&self.origin, "origin")?,
            captured_at: u64::try_from(self.captured_at)
                .map_err(|_| corrupt("capture time out of range"))?,
            training_use: from_json::<TrainingUse>(
                &format!("\"{}\"", self.training_use),
                "training use",
            )?,
        })
    }
}

pub(crate) fn corrupt(what: &str) -> MemoryError {
    MemoryError::Corrupt(what.to_owned())
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned>(
    json: &str,
    what: &str,
) -> Result<T, MemoryError> {
    serde_json::from_str(json).map_err(|e| MemoryError::Corrupt(format!("{what}: {e}")))
}

/// The bare name serde gives a unit-variant enum value, as stored in a column.
pub(crate) fn enum_name<T: serde::Serialize>(value: &T) -> Result<String, MemoryError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        _ => Err(MemoryError::Storage(
            "enum did not serialize to a name".into(),
        )),
    }
}

fn span(start: i64, end: i64) -> Result<Span, MemoryError> {
    let convert = |v: i64| usize::try_from(v).map_err(|_| corrupt("range out of bounds"));
    Ok(Span {
        start: convert(start)?,
        end: convert(end)?,
    })
}

/// Maps a failed row read to a storage error. Kept as a function so that every
/// `query_map` pipeline surfaces row errors the same way.
pub(crate) fn collect_rows<T>(
    rows: impl Iterator<Item = rusqlite::Result<T>>,
) -> Result<Vec<T>, MemoryError> {
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)
}
