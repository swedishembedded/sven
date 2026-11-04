// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an evaluation reads a frozen snapshot of memory. The snapshot has an
//! identity that follows content, nothing can be written through it, a store
//! that moved under it is refused rather than read, and every recall made
//! through it names the snapshot in the trajectory.
//!
//! Swedish Embedded AB implements reproducible evaluation of retrieval-assisted
//! agents for its clients. If your team needs expertise in measuring what an
//! agent's memory contributes then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
use sven_memory::{memory_snapshot_id, FrozenMemory, SemanticMemoryTool};
use sven_memory_api::{
    AccessContext, MemoryError, MemoryStore, Note, Origin, Permission, Provenance,
    RetrievalRequest, SourceId, SpaceId, TrainingUse,
};
use sven_memory_sqlite::SqliteMemory;
use sven_tool_api::call_records::{scope, CallRecorder};
use sven_tool_api::tool::{Tool, ToolCall};

fn space() -> SpaceId {
    SpaceId::new("kb")
}

fn owner() -> AccessContext {
    AccessContext::new("host").writing(space(), &[Permission::Ingest, Permission::Delete])
}

fn note(id: &str, text: &str) -> sven_memory_api::IngestRequest {
    Note {
        space: space(),
        source_id: SourceId::new(id),
        text,
        entity: None,
        source: None,
        tags: Vec::new(),
        provenance: Provenance {
            origin: Origin::User,
            captured_at: 1,
            training_use: TrainingUse::Unspecified,
        },
    }
    .into_request()
}

fn spaces() -> BTreeSet<SpaceId> {
    BTreeSet::from([space()])
}

async fn filled() -> Arc<SqliteMemory> {
    let store = Arc::new(SqliteMemory::open_in_memory().unwrap());
    store
        .ingest(&owner(), note("a", "alpha fact"))
        .await
        .unwrap();
    store
        .ingest(&owner(), note("b", "beta fact"))
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn the_snapshot_identity_follows_content_not_order_or_time() {
    let one = filled().await;
    let two = Arc::new(SqliteMemory::open_in_memory().unwrap());
    // The same content, ingested in the other order.
    two.ingest(&owner(), note("b", "beta fact")).await.unwrap();
    two.ingest(&owner(), note("a", "alpha fact")).await.unwrap();
    let id = |s: &Arc<SqliteMemory>| {
        let s = s.clone();
        async move {
            memory_snapshot_id(&*s, &owner(), &spaces(), None)
                .await
                .unwrap()
        }
    };
    assert_eq!(id(&one).await, id(&two).await);

    one.ingest(&owner(), note("c", "gamma fact")).await.unwrap();
    assert_ne!(
        id(&one).await,
        id(&two).await,
        "a new source is a new snapshot"
    );
    let before = id(&two).await;
    two.ingest(&owner(), note("a", "alpha fact, corrected"))
        .await
        .unwrap();
    assert_ne!(
        before,
        id(&two).await,
        "a corrected source is a new snapshot"
    );
}

#[tokio::test]
async fn a_frozen_memory_reads_but_never_writes_and_refuses_a_store_that_moved() {
    let store = filled().await;
    let frozen = FrozenMemory::freeze(store.clone(), &owner(), spaces(), None)
        .await
        .expect("freezes");
    let hits = frozen
        .retrieve(&owner(), RetrievalRequest::new("alpha", 3))
        .await
        .expect("reads")
        .hits;
    assert_eq!(hits.len(), 1);

    let write = frozen.ingest(&owner(), note("z", "new fact")).await;
    assert!(matches!(write, Err(MemoryError::Forbidden(_))), "{write:?}");
    let delete = frozen
        .delete_source(&owner(), &space(), &SourceId::new("a"))
        .await;
    assert!(
        matches!(delete, Err(MemoryError::Forbidden(_))),
        "{delete:?}"
    );

    // Someone writes to the underlying store: the snapshot no longer describes it.
    store
        .ingest(&owner(), note("c", "gamma fact"))
        .await
        .unwrap();
    let moved = frozen
        .retrieve(&owner(), RetrievalRequest::new("alpha", 3))
        .await;
    assert!(
        matches!(moved, Err(MemoryError::Unavailable(_))),
        "{moved:?}"
    );
}

#[tokio::test]
async fn a_recall_through_a_frozen_memory_names_the_snapshot_in_the_trajectory() {
    let store = filled().await;
    let frozen = Arc::new(
        FrozenMemory::freeze(store, &owner(), spaces(), None)
            .await
            .unwrap(),
    );
    let snapshot = frozen.snapshot_id().to_owned();
    let tool = SemanticMemoryTool::new(frozen, owner(), space());
    let recorder = CallRecorder::default();
    let out = scope(
        recorder.clone(),
        tool.execute(&ToolCall {
            id: "c".into(),
            name: "semantic_memory".into(),
            args: json!({"action": "recall", "query": "alpha"}),
        }),
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    let record = recorder.memory_retrievals().pop().expect("a record");
    assert_eq!(record.snapshot_id.as_deref(), Some(snapshot.as_str()));
}
