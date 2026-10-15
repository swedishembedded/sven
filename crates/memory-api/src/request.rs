// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Requests and responses of the recording and retrieval API.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    GenerationId, MemoryError, NewUnit, Provenance, RevisionId, SourceId, SpaceId, Unit, UnitId,
};

/// Largest source text a store accepts, in bytes.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Most units one source may be cut into.
pub const MAX_UNITS_PER_SOURCE: usize = 20_000;
/// Longest retrieval query, in bytes.
pub const MAX_QUERY_BYTES: usize = 4096;
/// Most tags one source may carry.
pub const MAX_TAGS: usize = 64;

/// A source to record, already cut into units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestRequest {
    /// The space it goes into.
    pub space: SpaceId,
    /// Its id within the space.
    pub source_id: SourceId,
    /// A human-readable label.
    pub label: String,
    /// The full text; its digest is the revision.
    pub text: String,
    /// Exact tags, for filtering.
    pub tags: BTreeSet<String>,
    /// Where it came from.
    pub provenance: Provenance,
    /// The units cut from `text`.
    pub units: Vec<NewUnit>,
}

impl IngestRequest {
    /// Checks everything that does not depend on stored state, so each
    /// adapter refuses the same inputs.
    ///
    /// A unit must be a verbatim slice of the text, by both byte and
    /// character range: that is what makes a stored unit trustworthy as a
    /// quotation of its source.
    ///
    /// # Errors
    /// [`MemoryError::InvalidInput`] naming the first violation.
    pub fn validate(&self) -> Result<(), MemoryError> {
        let invalid = |reason: String| Err(MemoryError::InvalidInput(reason));
        if self.source_id.as_str().is_empty() || self.space.as_str().is_empty() {
            return invalid("space and source ids must not be empty".into());
        }
        if self.text.len() > MAX_SOURCE_BYTES {
            return invalid(format!("source text exceeds {MAX_SOURCE_BYTES} bytes"));
        }
        if self.units.len() > MAX_UNITS_PER_SOURCE {
            return invalid(format!("more than {MAX_UNITS_PER_SOURCE} units"));
        }
        if self.tags.len() > MAX_TAGS {
            return invalid(format!("more than {MAX_TAGS} tags"));
        }
        for (ordinal, unit) in self.units.iter().enumerate() {
            let bytes = self.text.get(unit.byte_range.start..unit.byte_range.end);
            if bytes != Some(unit.text.as_str()) {
                return invalid(format!(
                    "unit {ordinal}: byte range is not a verbatim slice of the source text"
                ));
            }
            let chars = unit.char_range.end.checked_sub(unit.char_range.start);
            let start_char = self.text[..unit.byte_range.start].chars().count();
            if chars != Some(unit.text.chars().count()) || start_char != unit.char_range.start {
                return invalid(format!(
                    "unit {ordinal}: character range disagrees with the byte range"
                ));
            }
        }
        Ok(())
    }

    /// The revision this request would record.
    #[must_use]
    pub fn revision(&self) -> RevisionId {
        RevisionId::of_text(&self.text)
    }
}

/// What recording a source did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum IngestOutcome {
    /// The source was new.
    Created {
        /// The recorded revision.
        revision: RevisionId,
        /// The ids of its units, in order.
        units: Vec<UnitId>,
    },
    /// The source existed at an older revision and its units were replaced.
    Replaced {
        /// The revision now recorded.
        revision: RevisionId,
        /// The revision that was replaced.
        previous: RevisionId,
        /// The ids of its units, in order.
        units: Vec<UnitId>,
    },
    /// The source was already recorded at this revision; nothing was written.
    Unchanged {
        /// The recorded revision.
        revision: RevisionId,
        /// The ids of its units, in order.
        units: Vec<UnitId>,
    },
}

impl IngestOutcome {
    /// The revision now recorded.
    #[must_use]
    pub fn revision(&self) -> &RevisionId {
        match self {
            Self::Created { revision, .. }
            | Self::Replaced { revision, .. }
            | Self::Unchanged { revision, .. } => revision,
        }
    }

    /// The unit ids of the recorded source.
    #[must_use]
    pub fn units(&self) -> &[UnitId] {
        match self {
            Self::Created { units, .. }
            | Self::Replaced { units, .. }
            | Self::Unchanged { units, .. } => units,
        }
    }
}

/// A source as listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    /// Its id within the space.
    pub source_id: SourceId,
    /// Its label.
    pub label: String,
    /// Its current revision.
    pub revision: RevisionId,
    /// Its tags.
    pub tags: BTreeSet<String>,
    /// Where it came from.
    pub provenance: Provenance,
    /// How many units it has.
    pub unit_count: usize,
}

/// A search over the caller's spaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalRequest {
    /// Free text. Never interpreted as query syntax.
    pub query: String,
    /// Restricts the search to these spaces; they must all be readable. All
    /// readable spaces when `None`.
    pub spaces: Option<BTreeSet<SpaceId>>,
    /// Only sources carrying exactly this tag.
    pub tag: Option<String>,
    /// Most hits wanted; the caller's budget caps it.
    pub limit: usize,
    /// The index generation the query reads. It must be active in the store;
    /// otherwise the query is refused.
    pub generation: Option<GenerationId>,
}

impl RetrievalRequest {
    /// A lexical query over every readable space.
    #[must_use]
    pub fn new(query: impl Into<String>, limit: usize) -> Self {
        Self {
            query: query.into(),
            spaces: None,
            tag: None,
            limit,
            generation: None,
        }
    }

    /// Checks the request independently of stored state.
    ///
    /// # Errors
    /// [`MemoryError::InvalidInput`] for an oversized query.
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.query.len() > MAX_QUERY_BYTES {
            return Err(MemoryError::InvalidInput(format!(
                "query exceeds {MAX_QUERY_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

/// One retrieved unit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalHit {
    /// The space it was found in.
    pub space: SpaceId,
    /// The unit.
    pub unit: Unit,
    /// How its source entered memory, which decides how it is framed.
    pub provenance: Provenance,
    /// Relevance; higher is better, the scale belongs to the adapter.
    pub score: f32,
    /// Position in the response, starting at 1.
    pub rank: usize,
}

/// The answer to a [`RetrievalRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResponse {
    /// Hits in rank order; ties are broken by unit id.
    pub hits: Vec<RetrievalHit>,
    /// The spaces that were searched.
    pub spaces: BTreeSet<SpaceId>,
    /// The generation that was read, if the request named one.
    pub generation: Option<GenerationId>,
}

/// Schema tag of [`RetrievalRecord`].
pub const RETRIEVAL_RECORD_SCHEMA: &str = "sven.memory.v1";

/// One retrieved unit as written to a trajectory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievedUnitRecord {
    /// The unit.
    pub unit_id: UnitId,
    /// The revision it was cut from.
    pub revision: RevisionId,
    /// Where it sits.
    pub anchor: crate::Anchor,
    /// Its score.
    pub score: f32,
    /// Its rank.
    pub rank: usize,
    /// Whether it was put into the model's context.
    pub inserted: bool,
}

/// What a retrieval looked like, in the form a trajectory carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalRecord {
    /// Always [`RETRIEVAL_RECORD_SCHEMA`].
    pub schema: String,
    /// The query text.
    pub query: String,
    /// The spaces searched.
    pub space_ids: BTreeSet<SpaceId>,
    /// The generation read.
    pub generation_id: Option<GenerationId>,
    /// Pins the store state the answer came from, when the store has one.
    pub snapshot_id: Option<String>,
    /// The hits.
    pub results: Vec<RetrievedUnitRecord>,
}

impl RetrievalRecord {
    /// Records `response` to `request`; `inserted` says which units were put
    /// into the context.
    #[must_use]
    pub fn new(
        request: &RetrievalRequest,
        response: &RetrievalResponse,
        inserted: impl Fn(&UnitId) -> bool,
    ) -> Self {
        Self {
            schema: RETRIEVAL_RECORD_SCHEMA.to_owned(),
            query: request.query.clone(),
            space_ids: response.spaces.clone(),
            generation_id: response.generation.clone(),
            snapshot_id: None,
            results: response
                .hits
                .iter()
                .map(|hit| RetrievedUnitRecord {
                    unit_id: hit.unit.unit_id.clone(),
                    revision: hit.unit.revision.clone(),
                    anchor: hit.unit.anchor.clone(),
                    score: hit.score,
                    rank: hit.rank,
                    inserted: inserted(&hit.unit.unit_id),
                })
                .collect(),
        }
    }
}
