// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The annotation log: what agents and people say about units.
//!
//! The log is append-only. A store records proposals and the evidence behind
//! them and never decides what is true; admitting a claim is the host's
//! policy.

use serde::{Deserialize, Serialize};

use crate::{AnnotationId, RevisionId, UnitAnchor};

/// What an annotation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnnotationKind {
    /// A remark.
    Note,
    /// A correction of what the unit says.
    Correction,
    /// A proposed claim for the host to admit or refuse.
    ClaimProposal,
    /// A dispute of an earlier annotation or of the unit.
    Challenge,
    /// A link to another unit.
    Link,
}

/// A passage that supports an annotation, quoted verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The unit the passage is in.
    pub unit: UnitAnchor,
    /// The passage, exactly as it appears in the unit.
    pub span: String,
}

/// An annotation as submitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAnnotation {
    /// The unit it is about.
    pub target: UnitAnchor,
    /// What it does.
    pub kind: AnnotationKind,
    /// Its text.
    pub body: String,
    /// What supports it.
    pub evidence: Vec<Evidence>,
    /// The revision of the target's annotation log that the author read, as
    /// returned by [`crate::MemoryStore::annotation_head`] or by the previous
    /// append. A store refuses the annotation with
    /// [`crate::MemoryError::Conflict`] when the target has moved on.
    pub expected_revision: RevisionId,
}

/// An annotation as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    /// Its id.
    pub annotation_id: AnnotationId,
    /// The caller that appended it.
    pub author: String,
    /// What was submitted.
    pub content: NewAnnotation,
    /// The target's log revision after this annotation; the next author's
    /// `expected_revision`.
    pub resulting_revision: RevisionId,
}
