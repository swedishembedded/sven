// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cases about index generations and embeddings.

use super::fixtures::{owner_of, source, space};
use crate::{
    EmbeddingSpec, GenerationId, GenerationState, MemoryError, MemoryStore, PromptStyle,
    RetrievalRequest,
};

fn spec(weights: &str) -> EmbeddingSpec {
    EmbeddingSpec {
        model_id: "test-embedder".into(),
        weights_digest: weights.into(),
        tokenizer_digest: "tok".into(),
        pooling: "mean".into(),
        normalized: true,
        dims: 3,
        prompt: PromptStyle::Plain,
        max_tokens: 256,
    }
}

fn generation_request(generation: &GenerationId) -> RetrievalRequest {
    let mut request = RetrievalRequest::new("alpha", 5);
    request.generation = Some(generation.clone());
    request
}

/// A query may only read a generation that is active here.
pub async fn query_against_a_retired_or_foreign_generation_is_refused<S: MemoryStore>(store: &S) {
    let ctx = owner_of(&["s"]);
    store
        .ingest(&ctx, source("s", "doc", "alpha beta"))
        .await
        .expect("ingest");

    let registered = store
        .register_generation(spec("w1"), "seg-1")
        .await
        .expect("register");
    assert_eq!(registered.state, GenerationState::Building);
    assert_eq!(
        registered.generation_id,
        spec("w1").generation_id("seg-1"),
        "the id follows the contract's derivation"
    );
    assert_eq!(
        store
            .register_generation(spec("w1"), "seg-1")
            .await
            .expect("register again")
            .generation_id,
        registered.generation_id,
        "registering the same spec is idempotent"
    );
    let refused = |result: Result<crate::RetrievalResponse, MemoryError>| {
        matches!(result, Err(MemoryError::GenerationRefused { .. }))
    };

    let id = registered.generation_id;
    assert!(
        refused(store.retrieve(&ctx, generation_request(&id)).await),
        "building"
    );
    assert!(
        refused(
            store
                .retrieve(
                    &ctx,
                    generation_request(&GenerationId::new("not-registered"))
                )
                .await
        ),
        "foreign"
    );

    store
        .set_generation_state(&id, GenerationState::Active)
        .await
        .expect("activate");
    let response = store
        .retrieve(&ctx, generation_request(&id))
        .await
        .expect("an active generation is served");
    assert_eq!(response.generation, Some(id.clone()));

    store
        .set_generation_state(&id, GenerationState::Retired)
        .await
        .expect("retire");
    assert!(
        refused(store.retrieve(&ctx, generation_request(&id)).await),
        "retired"
    );
    assert!(
        matches!(
            store
                .set_generation_state(&id, GenerationState::Active)
                .await,
            Err(MemoryError::GenerationRefused { .. })
        ),
        "retirement is final"
    );
}

/// Vectors must fit the spec of the generation they are stored under.
pub async fn embeddings_must_match_the_generation_spec<S: MemoryStore>(store: &S) {
    let ctx = owner_of(&["s"]);
    let unit = store
        .ingest(&ctx, source("s", "doc", "alpha beta"))
        .await
        .expect("ingest")
        .units()[0]
        .clone();
    let generation = store
        .register_generation(spec("w2"), "seg-1")
        .await
        .expect("register")
        .generation_id;
    let put = |vector: Vec<f32>, unit: crate::UnitId| async {
        store
            .put_embeddings(&ctx, &space("s"), &generation, vec![(unit, vector)])
            .await
    };

    assert!(matches!(
        put(vec![0.1, 0.2], unit.clone()).await,
        Err(MemoryError::InvalidInput(_))
    ));
    assert!(matches!(
        put(vec![0.1, f32::NAN, 0.3], unit.clone()).await,
        Err(MemoryError::InvalidInput(_))
    ));
    assert!(matches!(
        put(vec![0.1, 0.2, 0.3], crate::UnitId::new("missing")).await,
        Err(MemoryError::NotFound)
    ));
    put(vec![0.1, 0.2, 0.3], unit)
        .await
        .expect("a fitting vector");

    let unknown = GenerationId::new("unknown");
    assert!(matches!(
        store
            .put_embeddings(&ctx, &space("s"), &unknown, Vec::new())
            .await,
        Err(MemoryError::GenerationRefused { .. })
    ));
}
