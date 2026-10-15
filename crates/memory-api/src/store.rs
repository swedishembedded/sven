// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The storage contract.

use async_trait::async_trait;

use crate::{
    AccessContext, Annotation, EmbeddingSpec, GenerationId, GenerationState, IndexGeneration,
    IngestOutcome, IngestRequest, MemoryError, NewAnnotation, RetrievalRequest, RetrievalResponse,
    RevisionId, SourceId, SourceSummary, SpaceId, Unit, UnitAnchor, UnitId,
};

/// Scoped memory storage.
///
/// Every call that touches stored content takes the caller's
/// [`AccessContext`]. An implementation applies it inside its own queries,
/// so a hit the caller may not see can neither be returned nor crowd out one
/// it may see. A space outside the caller's read set behaves as if it did not
/// exist. Any implementation must pass the conformance suite shipped with
/// this crate (feature `conformance`).
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Records `request.source_id` in `request.space`.
    ///
    /// Idempotent by content: a source already recorded at the same revision
    /// is left alone and reported as [`IngestOutcome::Unchanged`]. A changed
    /// revision replaces the source's units; its annotations stay.
    ///
    /// # Errors
    /// [`MemoryError::InvalidInput`] if [`IngestRequest::validate`] fails;
    /// [`MemoryError::Forbidden`] or [`MemoryError::NotFound`] if the caller
    /// may not ingest into the space.
    async fn ingest(
        &self,
        ctx: &AccessContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, MemoryError>;

    /// Lexical search over the caller's spaces.
    ///
    /// Spaces and tag filter apply inside the query, before the limit. Hits
    /// are ordered by descending score with ties broken by unit id.
    ///
    /// # Errors
    /// [`MemoryError::GenerationRefused`] if the request names a generation
    /// that is unknown or not active; [`MemoryError::NotFound`] if it names a
    /// space the caller cannot read.
    async fn retrieve(
        &self,
        ctx: &AccessContext,
        request: RetrievalRequest,
    ) -> Result<RetrievalResponse, MemoryError>;

    /// One unit by id, or `None` if it does not exist in a space the caller
    /// can read.
    async fn get_unit(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit_id: &UnitId,
    ) -> Result<Option<Unit>, MemoryError>;

    /// The sources of a space, newest ingestion last, optionally only those
    /// with exactly `tag`.
    async fn list_sources(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        tag: Option<&str>,
    ) -> Result<Vec<SourceSummary>, MemoryError>;

    /// Deletes a source with its units, index rows, embeddings and
    /// annotations. Returns whether anything was deleted.
    ///
    /// # Errors
    /// [`MemoryError::Forbidden`] or [`MemoryError::NotFound`] if the caller
    /// may not delete from the space.
    async fn delete_source(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        source_id: &SourceId,
    ) -> Result<bool, MemoryError>;

    /// The revision of the annotation log of `unit`: what the next
    /// [`NewAnnotation::expected_revision`] must hold. A unit without
    /// annotations is at its source's revision.
    ///
    /// # Errors
    /// [`MemoryError::NotFound`] if the unit is not in a readable space.
    async fn annotation_head(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<RevisionId, MemoryError>;

    /// Appends to the annotation log of the target unit.
    ///
    /// # Errors
    /// [`MemoryError::Conflict`] with the current revision if
    /// `expected_revision` is stale, whether because another annotation was
    /// appended or because the source was re-ingested.
    async fn append_annotation(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        annotation: NewAnnotation,
    ) -> Result<Annotation, MemoryError>;

    /// The annotations of `unit`, oldest first.
    async fn annotations(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<Vec<Annotation>, MemoryError>;

    /// Registers the generation for `spec` and `segmenter_version` in state
    /// [`GenerationState::Building`], or returns it if it exists.
    async fn register_generation(
        &self,
        spec: EmbeddingSpec,
        segmenter_version: &str,
    ) -> Result<IndexGeneration, MemoryError>;

    /// Moves a generation to `state`.
    ///
    /// # Errors
    /// [`MemoryError::GenerationRefused`] for an unknown generation or for
    /// leaving [`GenerationState::Retired`], which is final.
    async fn set_generation_state(
        &self,
        generation: &GenerationId,
        state: GenerationState,
    ) -> Result<IndexGeneration, MemoryError>;

    /// Stores embeddings of units of `generation`.
    ///
    /// # Errors
    /// [`MemoryError::GenerationRefused`] for an unknown or retired
    /// generation; [`MemoryError::InvalidInput`] if a vector's length is not
    /// the spec's `dims` or it holds a non-finite value;
    /// [`MemoryError::NotFound`] if a unit does not exist in the space.
    async fn put_embeddings(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        generation: &GenerationId,
        vectors: Vec<(UnitId, Vec<f32>)>,
    ) -> Result<(), MemoryError>;
}
