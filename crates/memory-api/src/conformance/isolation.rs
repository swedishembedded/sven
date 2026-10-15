// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cases about who may see and change what.

use super::fixtures::{owner_of, source, space};
use crate::{AccessContext, MemoryError, MemoryStore, RetrievalRequest};

/// A caller that cannot read the space holding the strongest matches still
/// gets the weaker match it can read: the scope is applied before the limit,
/// not to the hits that survive it.
pub async fn scope_filter_runs_inside_the_query_and_does_not_starve_visible_hits<S: MemoryStore>(
    store: &S,
) {
    let hidden = owner_of(&["hidden"]);
    for n in 0..30 {
        let text = "needle needle needle needle needle";
        store
            .ingest(&hidden, source("hidden", &format!("loud-{n}"), text))
            .await
            .expect("ingest into hidden");
    }
    let visible = owner_of(&["visible"]);
    store
        .ingest(
            &visible,
            source("visible", "quiet", "a single needle among many other words"),
        )
        .await
        .expect("ingest into visible");

    let response = store
        .retrieve(&visible, RetrievalRequest::new("needle", 3))
        .await
        .expect("retrieve");
    assert_eq!(
        response.hits.len(),
        1,
        "the one visible hit must be returned"
    );
    assert_eq!(response.hits[0].space, space("visible"));
    assert_eq!(response.hits[0].rank, 1);

    let both = owner_of(&["hidden", "visible"]);
    let response = store
        .retrieve(&both, RetrievalRequest::new("needle", 3))
        .await
        .expect("retrieve across both");
    assert_eq!(response.hits.len(), 3, "the limit applies to visible hits");
}

/// Another caller's space cannot be found, searched or emptied, and the same
/// source id in two spaces names two sources.
pub async fn a_unit_of_another_space_is_not_found_by_id_search_or_forget<S: MemoryStore>(
    store: &S,
) {
    let alice = owner_of(&["alice"]);
    let bob = owner_of(&["bob"]);
    let outcome = store
        .ingest(&alice, source("alice", "diary", "alice private secret"))
        .await
        .expect("ingest");
    let alice_unit = outcome.units()[0].clone();

    assert!(
        store
            .get_unit(&bob, &space("alice"), &alice_unit)
            .await
            .expect("get_unit")
            .is_none(),
        "a unit of an unreadable space is absent by id"
    );
    let response = store
        .retrieve(&bob, RetrievalRequest::new("secret", 10))
        .await
        .expect("retrieve");
    assert!(
        response.hits.is_empty(),
        "search must not reach another space"
    );
    let mut targeted = RetrievalRequest::new("secret", 10);
    targeted.spaces = Some([space("alice")].into());
    assert!(
        matches!(
            store.retrieve(&bob, targeted).await,
            Err(MemoryError::NotFound)
        ),
        "naming an unreadable space must look like naming a missing one"
    );
    assert!(
        matches!(
            store
                .delete_source(&bob, &space("alice"), &"diary".into())
                .await,
            Err(MemoryError::NotFound)
        ),
        "forgetting in an unreadable space must look like a missing space"
    );

    let outcome = store
        .ingest(&bob, source("bob", "diary", "alice private secret"))
        .await
        .expect("same source id in another space");
    assert_ne!(outcome.units()[0], alice_unit, "unit ids are space-scoped");
    assert!(store
        .delete_source(&bob, &space("bob"), &"diary".into())
        .await
        .expect("delete own"));
    assert!(
        store
            .get_unit(&alice, &space("alice"), &alice_unit)
            .await
            .expect("get_unit")
            .is_some(),
        "deleting bob's source must leave alice's alone"
    );
}

/// Reading a space does not allow changing it; each change needs its grant.
pub async fn writes_need_a_write_grant<S: MemoryStore>(store: &S) {
    let owner = owner_of(&["shared"]);
    store
        .ingest(&owner, source("shared", "doc", "shared knowledge"))
        .await
        .expect("owner ingests");

    let reader = AccessContext::new("reader").reading(space("shared"));
    assert!(matches!(
        store
            .ingest(&reader, source("shared", "other", "more"))
            .await,
        Err(MemoryError::Forbidden(_))
    ));
    assert!(matches!(
        store
            .delete_source(&reader, &space("shared"), &"doc".into())
            .await,
        Err(MemoryError::Forbidden(_))
    ));
    assert!(!store
        .retrieve(&reader, RetrievalRequest::new("knowledge", 5))
        .await
        .expect("reading is allowed")
        .hits
        .is_empty());
}
