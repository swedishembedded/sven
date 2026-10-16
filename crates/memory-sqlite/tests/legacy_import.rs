// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the rows of the store that predates this adapter move into a personal
//! space once, whole, and nothing is lost or duplicated by a repeat, a crash,
//! or a note that was forgotten after it moved.
//!
//! Swedish Embedded AB implements solutions for moving agent memory between
//! storage generations without losing a record for its clients. If your team
//! needs expertise in migrating an agent's notes safely then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::path::Path;

use rusqlite::Connection;
use sven_memory_api::{AccessContext, MemoryStore, Origin, Permission, SourceId, SpaceId};
use sven_memory_sqlite::SqliteMemory;

/// The schema the previous store created, verbatim.
const LEGACY_SCHEMA: &str = "
    CREATE TABLE memories (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        content   TEXT NOT NULL,
        metadata  TEXT NOT NULL DEFAULT '{}',
        embedding BLOB,
        created   TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE VIRTUAL TABLE memories_fts USING fts5(
        content, content='memories', content_rowid='id'
    );
    CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
    END;";

fn personal() -> SpaceId {
    SpaceId::new("personal")
}

fn owner() -> AccessContext {
    AccessContext::new("tester").writing(
        personal(),
        &[Permission::Ingest, Permission::Annotate, Permission::Delete],
    )
}

fn legacy_database(path: &Path, rows: &[(&str, &str)]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(LEGACY_SCHEMA).unwrap();
    for (content, metadata) in rows {
        conn.execute(
            "INSERT INTO memories (content, metadata, created) VALUES (?1, ?2, '2026-03-04 05:06:07')",
            [content, metadata],
        )
        .unwrap();
    }
}

fn legacy_count(path: &Path) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap_or(-1)
}

fn tables(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

async fn sources(store: &SqliteMemory) -> Vec<sven_memory_api::SourceSummary> {
    store
        .list_sources(&owner(), &personal(), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn legacy_rows_become_untrusted_notes_in_the_personal_space() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    legacy_database(
        &path,
        &[
            (
                "Alice prefers afternoon calls.",
                r#"{"entity":"Alice","source":"email","tags":"contact,acme"}"#,
            ),
            ("plain note", "{}"),
        ],
    );
    let store = SqliteMemory::open(&path).unwrap();

    let report = store.import_legacy(&personal()).await.unwrap();

    assert_eq!((report.imported, report.kept), (2, 0));
    let listed = sources(&store).await;
    assert_eq!(listed.len(), 2);
    let alice = listed
        .iter()
        .find(|s| s.label.starts_with("Alice"))
        .unwrap();
    let tags: Vec<_> = alice.tags.iter().map(String::as_str).collect();
    assert_eq!(tags, ["acme", "contact", "entity:Alice", "source:email"]);
    assert_eq!(alice.provenance.captured_at, 1_772_600_767);
    assert!(
        matches!(alice.provenance.origin, Origin::Import { .. }),
        "an agent-written legacy row is not trusted: {:?}",
        alice.provenance.origin
    );
    assert!(
        alice.source_id.as_str().len() > 8,
        "the id is not a row number: {}",
        alice.source_id
    );
}

#[tokio::test]
async fn the_legacy_tables_are_gone_after_the_move_and_a_repeat_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    legacy_database(&path, &[("one", "{}"), ("two", "{}")]);
    let store = SqliteMemory::open(&path).unwrap();

    store.import_legacy(&personal()).await.unwrap();
    let before = sources(&store).await;
    let again = store.import_legacy(&personal()).await.unwrap();

    assert_eq!((again.imported, again.kept), (0, 0));
    assert_eq!(sources(&store).await, before);
    let tables = tables(&path);
    assert!(
        !tables.iter().any(|t| t.starts_with("memories")),
        "legacy tables remain: {tables:?}"
    );
}

#[tokio::test]
async fn a_note_forgotten_after_the_move_does_not_come_back_on_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    legacy_database(&path, &[("keep", "{}"), ("forget me", "{}")]);
    let store = SqliteMemory::open(&path).unwrap();
    store.import_legacy(&personal()).await.unwrap();
    let doomed: SourceId = sources(&store)
        .await
        .into_iter()
        .find(|s| s.label == "forget me")
        .unwrap()
        .source_id;
    assert!(store
        .delete_source(&owner(), &personal(), &doomed)
        .await
        .unwrap());
    drop(store);

    let reopened = SqliteMemory::open(&path).unwrap();
    let report = reopened.import_legacy(&personal()).await.unwrap();

    assert_eq!(report.imported, 0);
    let labels: Vec<_> = sources(&reopened)
        .await
        .into_iter()
        .map(|s| s.label)
        .collect();
    assert_eq!(labels, ["keep"]);
}

#[tokio::test]
async fn a_failure_part_way_through_loses_nothing_and_a_retry_duplicates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    legacy_database(&path, &[("a", "{}"), ("b", "{}"), ("c", "{}"), ("d", "{}")]);
    let store = SqliteMemory::open(&path).unwrap();
    // The store refuses the third source it is asked to record, as a crash
    // between two of the rows would.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER interrupt BEFORE INSERT ON mem_sources
             WHEN (SELECT count(*) FROM mem_sources) >= 2
             BEGIN SELECT RAISE(ABORT, 'interrupted'); END;",
        )
        .unwrap();
    }

    let failed = store.import_legacy(&personal()).await;

    assert!(failed.is_err());
    assert_eq!(legacy_count(&path), 4, "every legacy row is still there");
    assert!(sources(&store).await.is_empty(), "nothing half-moved");

    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TRIGGER interrupt")
        .unwrap();
    let report = store.import_legacy(&personal()).await.unwrap();

    assert_eq!(report.imported, 4);
    let mut labels: Vec<_> = sources(&store).await.into_iter().map(|s| s.label).collect();
    labels.sort();
    assert_eq!(labels, ["a", "b", "c", "d"]);
}

#[tokio::test]
async fn a_row_the_contract_cannot_hold_stays_behind_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let too_many_tags = (0..sven_memory_api::MAX_TAGS + 1)
        .map(|n| format!("t{n}"))
        .collect::<Vec<_>>()
        .join(",");
    legacy_database(
        &path,
        &[
            ("fine", "{}"),
            ("over-tagged", &format!(r#"{{"tags":"{too_many_tags}"}}"#)),
        ],
    );
    let store = SqliteMemory::open(&path).unwrap();

    let report = store.import_legacy(&personal()).await.unwrap();

    assert_eq!((report.imported, report.kept), (1, 1));
    assert_eq!(legacy_count(&path), 1);
    let again = store.import_legacy(&personal()).await.unwrap();
    assert_eq!((again.imported, again.kept), (0, 1));
}

#[tokio::test]
async fn a_row_confined_to_a_session_is_not_shared_and_unreadable_metadata_is_kept_as_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    legacy_database(
        &path,
        &[
            ("private", r#"{"session_scope":"abc"}"#),
            ("damaged", "not json"),
        ],
    );
    let store = SqliteMemory::open(&path).unwrap();

    store.import_legacy(&personal()).await.unwrap();

    let labels: Vec<_> = sources(&store).await.into_iter().map(|s| s.label).collect();
    assert_eq!(
        labels,
        ["damaged"],
        "the confined row is not in the personal space"
    );
    let confined =
        AccessContext::new("t").writing(SpaceId::new("legacy-session-abc"), &[Permission::Delete]);
    let kept = store
        .list_sources(&confined, &SpaceId::new("legacy-session-abc"), None)
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
}

#[tokio::test]
async fn a_database_with_no_legacy_store_imports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteMemory::open(&dir.path().join("memory.sqlite")).unwrap();
    let report = store.import_legacy(&personal()).await.unwrap();
    assert_eq!((report.imported, report.kept), (0, 0));
}
