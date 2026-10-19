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

/// Registers and activates a generation for `spec("w")`.
async fn active_generation<S: MemoryStore>(store: &S) -> GenerationId {
    let id = store
        .register_generation(spec("w"), "seg-1")
        .await
        .expect("register")
        .generation_id;
    store
        .set_generation_state(&id, GenerationState::Active)
        .await
        .expect("activate");
    id
}

/// With a query vector the search reads by meaning: a unit that shares no word
/// with the query is found by its embedding, one that does share words keeps
/// its place, and the order is the same every time.
pub async fn a_query_vector_finds_by_meaning_what_no_word_matches<S: MemoryStore>(store: &S) {
    let ctx = owner_of(&["s"]);
    let units = store
        .ingest(
            &ctx,
            source("s", "doc", "alpha unit\n\nbeta unit\n\ngamma unit"),
        )
        .await
        .expect("ingest")
        .units()
        .to_vec();
    let generation = active_generation(store).await;
    store
        .put_embeddings(
            &ctx,
            &space("s"),
            &generation,
            vec![
                (units[0].clone(), vec![1.0, 0.0, 0.0]),
                (units[1].clone(), vec![0.0, 1.0, 0.0]),
                (units[2].clone(), vec![0.0, 0.0, 1.0]),
            ],
        )
        .await
        .expect("embed");

    // No unit contains this word; the vector points at the second.
    let query =
        RetrievalRequest::new("zzzz", 3).with_embedding(generation.clone(), vec![0.1, 0.9, 0.0]);
    let first = store.retrieve(&ctx, query.clone()).await.expect("hybrid");
    assert_eq!(first.hits[0].unit.unit_id, units[1], "found by meaning");
    assert_eq!(first.hits[0].rank, 1);
    let again = store.retrieve(&ctx, query).await.expect("hybrid again");
    let order = |r: &crate::RetrievalResponse| -> Vec<crate::UnitId> {
        r.hits.iter().map(|h| h.unit.unit_id.clone()).collect()
    };
    assert_eq!(order(&first), order(&again), "the same order every time");

    // A word that does match still counts: it outranks a weak vector match.
    let query =
        RetrievalRequest::new("gamma", 3).with_embedding(generation.clone(), vec![1.0, 0.0, 0.0]);
    let hits = store.retrieve(&ctx, query).await.expect("hybrid").hits;
    assert!(
        hits.iter().any(|h| h.unit.unit_id == units[2]),
        "the lexical hit is in the fused list"
    );
    assert!(
        hits.iter().any(|h| h.unit.unit_id == units[0]),
        "and so is the nearest vector"
    );
}

/// A query vector reads only what the caller may read, and what it may not
/// read cannot crowd out what it may.
pub async fn a_query_vector_stays_inside_the_callers_spaces<S: MemoryStore>(store: &S) {
    let owner = owner_of(&["mine", "theirs"]);
    let mine = store
        .ingest(&owner, source("mine", "doc", "ordinary note"))
        .await
        .expect("ingest")
        .units()[0]
        .clone();
    let theirs = store
        .ingest(&owner, source("theirs", "doc", "secret note"))
        .await
        .expect("ingest")
        .units()[0]
        .clone();
    let generation = active_generation(store).await;
    // The unit the caller may not read is the nearest to the query.
    store
        .put_embeddings(
            &owner,
            &space("mine"),
            &generation,
            vec![(mine.clone(), vec![0.6, 0.8, 0.0])],
        )
        .await
        .expect("embed mine");
    store
        .put_embeddings(
            &owner,
            &space("theirs"),
            &generation,
            vec![(theirs, vec![1.0, 0.0, 0.0])],
        )
        .await
        .expect("embed theirs");

    let reader = owner_of(&["mine"]);
    let query = RetrievalRequest::new("zzzz", 1).with_embedding(generation, vec![1.0, 0.0, 0.0]);
    let hits = store.retrieve(&reader, query).await.expect("hybrid").hits;
    assert_eq!(
        hits.len(),
        1,
        "the unreadable nearer unit did not take the only place"
    );
    assert_eq!(hits[0].unit.unit_id, mine);
}

/// A query vector must fit the generation it names.
pub async fn a_query_vector_must_fit_the_generation<S: MemoryStore>(store: &S) {
    let ctx = owner_of(&["s"]);
    store
        .ingest(&ctx, source("s", "doc", "alpha"))
        .await
        .expect("ingest");
    let generation = active_generation(store).await;
    let wrong_size =
        RetrievalRequest::new("alpha", 3).with_embedding(generation.clone(), vec![1.0, 0.0]);
    assert!(matches!(
        store.retrieve(&ctx, wrong_size).await,
        Err(MemoryError::InvalidInput(_))
    ));
    let mut no_generation = RetrievalRequest::new("alpha", 3);
    no_generation.query_embedding = Some(vec![1.0, 0.0, 0.0]);
    assert!(matches!(
        store.retrieve(&ctx, no_generation).await,
        Err(MemoryError::InvalidInput(_))
    ));
}
