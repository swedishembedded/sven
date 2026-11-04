// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Behavior that belongs to the SQLite engine rather than to the contract:
//! the schema migrator, where the database goes, damaged rows, and what a
//! delete leaves behind in the index tables.
//!
//! Swedish Embedded AB implements solutions for embedded, tenant-isolated
//! agent memory for its clients. If your team needs expertise in putting
//! searchable agent memory into a product on SQLite then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;

use rusqlite::Connection;
use sven_memory_api::{
    AccessContext, Anchor, IngestRequest, MemoryError, MemoryStore, NewUnit, Origin, Permission,
    Provenance, RetrievalRequest, SourceId, SpaceId, Span, TrainingUse, UnitKind,
};
use sven_memory_sqlite::{SqliteMemory, SCHEMA_VERSION};

fn owner() -> AccessContext {
    AccessContext::new("tester").writing(
        SpaceId::new("s"),
        &[Permission::Ingest, Permission::Annotate, Permission::Delete],
    )
}

fn request(source: &str, text: &str) -> IngestRequest {
    IngestRequest {
        space: SpaceId::new("s"),
        source_id: SourceId::new(source),
        label: source.to_owned(),
        text: text.to_owned(),
        tags: Default::default(),
        provenance: Provenance {
            origin: Origin::User,
            captured_at: 1,
            training_use: TrainingUse::Unspecified,
        },
        units: vec![NewUnit {
            kind: UnitKind::Prose,
            anchor: Anchor {
                heading_path: Vec::new(),
                block_ordinal: 0,
                text_fingerprint: "f".into(),
            },
            byte_range: Span {
                start: 0,
                end: text.len(),
            },
            char_range: Span {
                start: 0,
                end: text.chars().count(),
            },
            text: text.to_owned(),
        }],
    }
}

fn count(path: &Path, table: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// A database as the store wrote it before versioning: legacy tables, one
/// row, `user_version` 0.
fn write_v0_fixture(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (
             id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL,
             metadata TEXT NOT NULL DEFAULT '{}', embedding BLOB,
             created TEXT NOT NULL DEFAULT (datetime('now')));
         CREATE VIRTUAL TABLE memories_fts USING fts5(
             content, content='memories', content_rowid='id');
         INSERT INTO memories (content) VALUES ('legacy fact');",
    )
    .unwrap();
}

#[tokio::test]
async fn migrates_v0_fixture_and_is_idempotent_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    write_v0_fixture(&path);

    let store = SqliteMemory::open(&path).unwrap();
    store
        .ingest(&owner(), request("doc", "fresh knowledge"))
        .await
        .unwrap();
    drop(store);

    let version = |p: &Path| -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(version(&path), SCHEMA_VERSION);
    assert_eq!(count(&path, "memories"), 1, "legacy rows are left in place");

    let reopened = SqliteMemory::open(&path).unwrap();
    let hits = reopened
        .retrieve(&owner(), RetrievalRequest::new("fresh", 5))
        .await
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1, "data survives a reopen");
    assert_eq!(version(&path), SCHEMA_VERSION);
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    drop(SqliteMemory::open(&path).unwrap());
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        SqliteMemory::open(&path),
        Err(MemoryError::Unavailable(_))
    ));
}

#[test]
fn open_without_home_fails_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        SqliteMemory::open_in_home(None),
        Err(MemoryError::Unavailable(_))
    ));
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "nothing is created anywhere on a failed open"
    );
    let path = SqliteMemory::default_path(Some(dir.path())).unwrap();
    assert!(path.starts_with(dir.path()));
    SqliteMemory::open_in_home(Some(dir.path())).unwrap();
    assert!(path.exists());
}

#[tokio::test]
async fn row_errors_surface() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let store = SqliteMemory::open(&path).unwrap();
    store
        .ingest(&owner(), request("doc", "intact words here"))
        .await
        .unwrap();
    store
        .ingest(&owner(), request("other", "intact words there"))
        .await
        .unwrap();

    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE mem_units SET heading_path = 'not json' \
             WHERE source_row = (SELECT id FROM mem_sources WHERE source_id = 'doc')",
            [],
        )
        .unwrap();

    let result = store
        .retrieve(&owner(), RetrievalRequest::new("words", 10))
        .await;
    assert!(
        matches!(result, Err(MemoryError::Corrupt(_))),
        "a damaged row must fail the query, not vanish from it: {result:?}"
    );
}

#[tokio::test]
async fn delete_leaves_no_rows_in_any_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let store = SqliteMemory::open(&path).unwrap();
    let ctx = owner();
    let outcome = store
        .ingest(&ctx, request("doc", "indexed words"))
        .await
        .unwrap();
    let generation = store
        .register_generation(
            sven_memory_api::EmbeddingSpec {
                model_id: "m".into(),
                weights_digest: "w".into(),
                tokenizer_digest: "t".into(),
                pooling: "mean".into(),
                normalized: true,
                dims: 2,
                prompt: sven_memory_api::PromptStyle::Plain,
                max_tokens: 8,
            },
            "seg",
        )
        .await
        .unwrap();
    store
        .put_embeddings(
            &ctx,
            &SpaceId::new("s"),
            &generation.generation_id,
            vec![(outcome.units()[0].clone(), vec![0.5, 0.5])],
        )
        .await
        .unwrap();
    assert_eq!(count(&path, "mem_embeddings"), 1);

    assert!(store
        .delete_source(&ctx, &SpaceId::new("s"), &SourceId::new("doc"))
        .await
        .unwrap());

    for table in [
        "mem_sources",
        "mem_units",
        "mem_source_tags",
        "mem_embeddings",
    ] {
        assert_eq!(count(&path, table), 0, "{table} still holds rows");
    }
    let indexed: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM mem_units_fts WHERE mem_units_fts MATCH 'indexed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0, "the full-text index still finds deleted text");
}
