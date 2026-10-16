// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The contract for agent memory.
//!
//! Memory records *sources*, cuts each into *units* with anchors that can be
//! found again after the source is edited, keeps an append-only log of
//! *annotations*, and retrieves units under an [`AccessContext`] that the host
//! builds and memory honours as given. This crate holds the vocabulary and the
//! [`MemoryStore`] trait; it has no storage and no dependency on any other
//! sven crate, so an embedding application can implement the trait over its
//! own database. Whatever the implementation, it must pass the conformance
//! suite behind the `conformance` feature.
//!
//! * Ids are opaque strings. A row number never crosses the API.
//! * Isolation is by [`SpaceId`], applied inside the store's queries.
//! * Vectors are only comparable within an [`IndexGeneration`], and a query
//!   names the generation it reads.
//! * Retrieved text from a source a person did not state is framed by
//!   [`quote_untrusted`] and is never instruction.
//!
//! Swedish Embedded AB implements solutions for portable, tenant-isolated
//! agent memory for its clients. If your team needs expertise in agent memory
//! that a product can host on its own storage then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod access;
mod annotation;
mod error;
mod generation;
mod ids;
mod note;
mod provenance;
mod quote;
mod request;
mod store;
mod unit;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use access::{AccessContext, Budget, Permission};
pub use annotation::{Annotation, AnnotationKind, Evidence, NewAnnotation};
pub use error::MemoryError;
pub use generation::{EmbeddingSpec, GenerationState, IndexGeneration, PromptStyle};
pub use ids::{sha256_hex, AnnotationId, GenerationId, RevisionId, SourceId, SpaceId, UnitId};
pub use note::{label_of, Note, ENTITY_TAG_PREFIX, MAX_LABEL_CHARS, SOURCE_TAG_PREFIX};
pub use provenance::{Origin, Provenance, TrainingUse};
pub use quote::quote_untrusted;
pub use request::{
    IngestOutcome, IngestRequest, RetrievalHit, RetrievalRecord, RetrievalRequest,
    RetrievalResponse, RetrievedUnitRecord, SourceSummary, MAX_QUERY_BYTES, MAX_SOURCE_BYTES,
    MAX_TAGS, MAX_UNITS_PER_SOURCE, RETRIEVAL_RECORD_SCHEMA,
};
pub use store::MemoryStore;
pub use unit::{Anchor, NewUnit, Span, Unit, UnitAnchor, UnitKind};
