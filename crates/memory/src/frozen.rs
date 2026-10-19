// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements reproducible evaluation of retrieval-assisted
// agents for its clients. If your team needs expertise in measuring what an
// agent's memory contributes then you can procure our services by sending an
// email to info@swedishembedded.com.
//! A memory frozen for an evaluation.
//!
//! What a retrieval-assisted agent answers depends on what its memory held, so
//! a measurement has to say which memory, and it has to be the same memory every
//! time. [`memory_snapshot_id`] names a store's content; [`FrozenMemory`] pins a
//! store to one such name, reads through, writes nothing, and refuses to answer
//! once the store has moved.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use sven_memory_api::{
    sha256_hex, AccessContext, Annotation, EmbeddingSpec, GenerationId, GenerationState,
    IndexGeneration, IngestOutcome, IngestRequest, MemoryError, MemoryStore, NewAnnotation,
    RetrievalRequest, RetrievalResponse, RevisionId, SourceId, SourceSummary, SpaceId, Unit,
    UnitAnchor, UnitId,
};

/// The identity of the content `ctx` can read in `spaces`: the digest of every
/// source (space, id and content revision) in order, and of the index
/// generation when one is named. It follows content, not the order sources were
/// added in or when, so the same memory built twice has the same identity and
/// any added, changed or removed source changes it.
///
/// # Errors
/// The store's refusal to list a space.
pub async fn memory_snapshot_id(
    store: &dyn MemoryStore,
    ctx: &AccessContext,
    spaces: &BTreeSet<SpaceId>,
    generation: Option<&GenerationId>,
) -> Result<String, MemoryError> {
    let mut lines = Vec::new();
    for space in spaces {
        for source in store.list_sources(ctx, space, None).await? {
            lines.push(format!(
                "{space}\t{}\t{}",
                source.source_id, source.revision
            ));
        }
    }
    lines.sort();
    let generation = generation.map_or_else(String::new, ToString::to_string);
    let basis = format!("sven-memory-snapshot-1\n{generation}\n{}", lines.join("\n"));
    Ok(format!("snap-{}", &sha256_hex(basis.as_bytes())[..32]))
}

/// A store pinned to the content it had when it was frozen.
pub struct FrozenMemory {
    inner: Arc<dyn MemoryStore>,
    ctx: AccessContext,
    spaces: BTreeSet<SpaceId>,
    generation: Option<GenerationId>,
    snapshot: String,
}

impl FrozenMemory {
    /// Freezes what `ctx` can read in `spaces` of `inner` now.
    ///
    /// # Errors
    /// The store's refusal to list a space.
    pub async fn freeze(
        inner: Arc<dyn MemoryStore>,
        ctx: &AccessContext,
        spaces: BTreeSet<SpaceId>,
        generation: Option<GenerationId>,
    ) -> Result<Self, MemoryError> {
        let snapshot = memory_snapshot_id(&*inner, ctx, &spaces, generation.as_ref()).await?;
        Ok(Self {
            inner,
            ctx: ctx.clone(),
            spaces,
            generation,
            snapshot,
        })
    }

    /// The identity of the frozen content.
    #[must_use]
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot
    }

    async fn still_frozen(&self) -> Result<(), MemoryError> {
        let now = memory_snapshot_id(
            &*self.inner,
            &self.ctx,
            &self.spaces,
            self.generation.as_ref(),
        )
        .await?;
        if now == self.snapshot {
            Ok(())
        } else {
            Err(MemoryError::Unavailable(format!(
                "the memory changed since snapshot {}: it now holds {now}",
                self.snapshot
            )))
        }
    }
}

fn frozen() -> MemoryError {
    MemoryError::Forbidden("the memory is a frozen snapshot".into())
}

#[async_trait]
impl MemoryStore for FrozenMemory {
    fn frozen_snapshot(&self) -> Option<String> {
        Some(self.snapshot.clone())
    }

    async fn ingest(
        &self,
        _ctx: &AccessContext,
        _request: IngestRequest,
    ) -> Result<IngestOutcome, MemoryError> {
        Err(frozen())
    }

    async fn retrieve(
        &self,
        ctx: &AccessContext,
        request: RetrievalRequest,
    ) -> Result<RetrievalResponse, MemoryError> {
        self.still_frozen().await?;
        self.inner.retrieve(ctx, request).await
    }

    async fn get_unit(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit_id: &UnitId,
    ) -> Result<Option<Unit>, MemoryError> {
        self.still_frozen().await?;
        self.inner.get_unit(ctx, space, unit_id).await
    }

    async fn list_sources(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        tag: Option<&str>,
    ) -> Result<Vec<SourceSummary>, MemoryError> {
        self.inner.list_sources(ctx, space, tag).await
    }

    async fn delete_source(
        &self,
        _ctx: &AccessContext,
        _space: &SpaceId,
        _source_id: &SourceId,
    ) -> Result<bool, MemoryError> {
        Err(frozen())
    }

    async fn annotation_head(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<RevisionId, MemoryError> {
        self.inner.annotation_head(ctx, space, unit).await
    }

    async fn append_annotation(
        &self,
        _ctx: &AccessContext,
        _space: &SpaceId,
        _annotation: NewAnnotation,
    ) -> Result<Annotation, MemoryError> {
        Err(frozen())
    }

    async fn annotations(
        &self,
        ctx: &AccessContext,
        space: &SpaceId,
        unit: &UnitAnchor,
    ) -> Result<Vec<Annotation>, MemoryError> {
        self.inner.annotations(ctx, space, unit).await
    }

    async fn register_generation(
        &self,
        spec: EmbeddingSpec,
        segmenter_version: &str,
    ) -> Result<IndexGeneration, MemoryError> {
        self.inner
            .register_generation(spec, segmenter_version)
            .await
    }

    async fn set_generation_state(
        &self,
        _generation: &GenerationId,
        _state: GenerationState,
    ) -> Result<IndexGeneration, MemoryError> {
        Err(frozen())
    }

    async fn put_embeddings(
        &self,
        _ctx: &AccessContext,
        _space: &SpaceId,
        _generation: &GenerationId,
        _vectors: Vec<(UnitId, Vec<f32>)>,
    ) -> Result<(), MemoryError> {
        Err(frozen())
    }
}
