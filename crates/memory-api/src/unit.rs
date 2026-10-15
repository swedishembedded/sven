// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Units: the passages of a source that retrieval returns and annotations
//! point at.

use serde::{Deserialize, Serialize};

use crate::{RevisionId, SourceId, UnitId};

/// A half-open range `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// First position inside the range.
    pub start: usize,
    /// First position after the range.
    pub end: usize,
}

/// What a unit is, so a consumer can present it suitably.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnitKind {
    /// A paragraph or other run of prose.
    Prose,
    /// A heading.
    Heading,
    /// A block of source code.
    Code,
}

/// Where a unit sits in its source and what it says, so it can be found again
/// after the source is edited.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// The headings above the unit, outermost first.
    pub heading_path: Vec<String>,
    /// Position among the blocks under the same heading path.
    pub block_ordinal: u32,
    /// Digest of the unit's text.
    pub text_fingerprint: String,
}

/// A unit as supplied by a segmenter at ingestion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewUnit {
    /// What the unit is.
    pub kind: UnitKind,
    /// Where it sits.
    pub anchor: Anchor,
    /// Its extent in the source text, in bytes.
    pub byte_range: Span,
    /// Its extent in the source text, in characters.
    pub char_range: Span,
    /// The source text of that extent, verbatim.
    pub text: String,
}

/// A stored unit as retrieval returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    /// The unit's id.
    pub unit_id: UnitId,
    /// The source it belongs to.
    pub source_id: SourceId,
    /// The revision of that source it was cut from.
    pub revision: RevisionId,
    /// What the unit is.
    pub kind: UnitKind,
    /// Where it sits.
    pub anchor: Anchor,
    /// Its extent in the source text, in bytes.
    pub byte_range: Span,
    /// Its extent in the source text, in characters.
    pub char_range: Span,
    /// The unit's text.
    pub text: String,
}

/// A reference to a unit that names the revision it was read at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitAnchor {
    /// The source the unit belongs to.
    pub source_id: SourceId,
    /// The unit.
    pub unit_id: UnitId,
    /// The revision the reference was made at.
    pub revision: RevisionId,
    /// The unit's anchor at that revision, for finding it again.
    pub anchor: Anchor,
}

impl Unit {
    /// A reference to this unit as it is now.
    #[must_use]
    pub fn to_anchor(&self) -> UnitAnchor {
        UnitAnchor {
            source_id: self.source_id.clone(),
            unit_id: self.unit_id.clone(),
            revision: self.revision.clone(),
            anchor: self.anchor.clone(),
        }
    }
}
