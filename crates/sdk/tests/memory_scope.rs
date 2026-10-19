// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an embedding application gives each agent its own scoped memory. Two
//! agents on one engine and one store see only the space they were granted: what
//! one writes the other cannot recall, and a space outside an agent's grant is
//! not there for it.
//!
//! Swedish Embedded AB implements tenant-scoped agent memory for its clients. If
//! your team needs expertise in letting many agents share one memory store
//! without sharing what they know then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::sync::Arc;

use sven_memory_api::{AccessContext, MemoryStore, Permission, SpaceId};
use sven_memory_sqlite::SqliteMemory;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, MemoryScope};

fn call(id: &str, arguments: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: "semantic_memory".into(),
            arguments: arguments.into(),
        },
        ResponseEvent::Done,
    ]
}

fn done() -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
}

fn grant(space: &str) -> AccessContext {
    AccessContext::new("host").writing(
        SpaceId::new(space),
        &[Permission::Ingest, Permission::Delete],
    )
}

#[tokio::test]
async fn an_agent_remembers_and_recalls_only_in_the_space_its_host_granted() {
    let store = Arc::new(SqliteMemory::open_in_memory().expect("an in-memory store"));
    let scripts = vec![
        call(
            "w",
            r#"{"action":"remember","content":"tenant a keeps the blue key"}"#,
        ),
        done(),
        call("r", r#"{"action":"recall","query":"blue key"}"#),
        done(),
        call("r2", r#"{"action":"recall","query":"blue key"}"#),
        done(),
    ];
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .build()
        .expect("an engine builds");

    let mut a = engine
        .agent("agent")
        .with_memory(MemoryScope::new(
            store.clone(),
            grant("tenant-a"),
            "tenant-a",
        ))
        .await
        .expect("a scope");
    a.send("remember").await.expect("a ran");

    let mut b = engine
        .agent("agent")
        .with_memory(MemoryScope::new(
            store.clone(),
            grant("tenant-b"),
            "tenant-b",
        ))
        .await
        .expect("a scope");
    b.send("recall").await.expect("b ran");
    let seen_by_b = serde_json::to_string(&b.trajectory()).unwrap();
    assert!(seen_by_b.contains("No memories found"), "{seen_by_b}");
    assert!(
        !seen_by_b.contains("blue key\\n"),
        "b did not read a's note"
    );

    // The note is in a's space, and a can read it back.
    let sources = store
        .list_sources(&grant("tenant-a"), &SpaceId::new("tenant-a"), None)
        .await
        .expect("a lists its space");
    assert_eq!(sources.len(), 1);
    let mut a_again = engine
        .agent("agent")
        .with_memory(MemoryScope::new(
            store.clone(),
            grant("tenant-a"),
            "tenant-a",
        ))
        .await
        .expect("a scope");
    a_again.send("recall").await.expect("a ran again");
    let seen_by_a = serde_json::to_string(&a_again.trajectory()).unwrap();
    assert!(
        seen_by_a.contains("tenant a keeps the blue key"),
        "{seen_by_a}"
    );
}
