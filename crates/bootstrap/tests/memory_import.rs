// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: starting a session on a home that holds the previous memory store
//! moves its notes into the contract's spaces, with the trust the previous
//! store recorded, and drops the old table; the next start moves nothing.
//!
//! Swedish Embedded AB implements solutions for upgrading an agent's stored
//! memory in place without losing or mis-trusting a note for its clients. If
//! your team needs expertise in migrating agent state across releases then you
//! can procure our services by sending an email to info@swedishembedded.com.
#![cfg(feature = "memory")]

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use sven_bootstrap::{Config, RuntimeBuilder, RuntimeContext};
use sven_memory_api::{AccessContext, MemoryStore, Origin, Permission, SourceSummary, SpaceId};
use sven_memory_sqlite::SqliteMemory;

/// The schema the previous store created, as that store wrote it.
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

const SESSION: &str = "7f3a";

fn store_path(home: &Path) -> std::path::PathBuf {
    home.join(".config/sven/memory/memory.sqlite")
}

/// A home holding a previous-format store with one note of each kind.
fn home_with_previous_store() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a home directory");
    let path = store_path(home.path());
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a store directory");
    let conn = Connection::open(path).expect("a database");
    conn.execute_batch(LEGACY_SCHEMA)
        .expect("the previous schema");
    let notes = [
        (
            "The firmware build needs arm-none-eabi-gcc 13.",
            r#"{"entity":"firmware build","source":"agent","tags":"toolchain"}"#,
        ),
        (
            "Call Alice in the afternoon.",
            r#"{"provenance":"user_stated","entity":"Alice"}"#,
        ),
        (
            "scratch note for one session",
            r#"{"session_scope":"7f3a"}"#,
        ),
    ];
    for (content, metadata) in notes {
        conn.execute(
            "INSERT INTO memories (content, metadata, created) VALUES (?1, ?2, '2026-03-04 05:06:07')",
            [content, metadata],
        )
        .expect("a previous-store row");
    }
    home
}

/// Starts a session on `home` the way every surface does, then ends it.
async fn start_session(home: &Path) {
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    let (runtime, ..) = RuntimeBuilder::new(Arc::new(config), "chat")
        .with_runtime_context(RuntimeContext {
            memory_home: Some(home.to_path_buf()),
            ..RuntimeContext::empty()
        })
        .build()
        .await
        .expect("a runtime builds");
    runtime.abort();
}

async fn notes_in(home: &Path, space: &str) -> Vec<SourceSummary> {
    let space = SpaceId::new(space);
    let reader = AccessContext::new("reader").writing(space.clone(), &[Permission::Delete]);
    SqliteMemory::open(&store_path(home))
        .expect("the store opens")
        .list_sources(&reader, &space, None)
        .await
        .expect("a listing")
}

fn has_previous_table(home: &Path) -> bool {
    Connection::open(store_path(home))
        .expect("the database")
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'memories'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("a catalogue query")
        > 0
}

#[tokio::test]
async fn starting_a_session_moves_the_previous_notes_with_their_trust() {
    let home = home_with_previous_store();

    start_session(home.path()).await;

    let personal = notes_in(home.path(), "personal").await;
    assert_eq!(personal.len(), 2, "{personal:?}");
    let agent_written = personal
        .iter()
        .find(|note| note.label.starts_with("The firmware build"))
        .expect("the agent's note is in the personal space");
    assert!(
        matches!(agent_written.provenance.origin, Origin::Import { .. }),
        "an agent-written note stays untrusted: {:?}",
        agent_written.provenance.origin
    );
    let stated = personal
        .iter()
        .find(|note| note.label.starts_with("Call Alice"))
        .expect("the person's note is in the personal space");
    assert!(
        matches!(stated.provenance.origin, Origin::User),
        "a note a person stated is trusted: {:?}",
        stated.provenance.origin
    );

    let confined = notes_in(home.path(), &format!("legacy-session-{SESSION}")).await;
    assert_eq!(confined.len(), 1, "{confined:?}");
    assert!(confined[0].label.starts_with("scratch note"));
    assert!(!has_previous_table(home.path()));
}

#[tokio::test]
async fn a_second_start_moves_nothing() {
    let home = home_with_previous_store();
    start_session(home.path()).await;
    let first = notes_in(home.path(), "personal").await;

    start_session(home.path()).await;

    assert_eq!(notes_in(home.path(), "personal").await, first);
    assert_eq!(
        notes_in(home.path(), &format!("legacy-session-{SESSION}"))
            .await
            .len(),
        1
    );
    assert!(!has_previous_table(home.path()));
}
