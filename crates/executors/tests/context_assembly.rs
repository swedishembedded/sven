// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The executor side of addressable context: results are recorded as they
//! arrive, every demotion is backed by stored evidence, and what cannot be
//! stored is never demoted.

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::json;
use sven_chunks::{
    ChunkId, ChunkStore, Freshness, FsProbe, SharedChunkStore, SourceProbe, StoreLimits,
};
use sven_executors::{ChunkSession, ContextAssembler};
use sven_model::{FunctionCall, Message, MessageContent, Role, ToolResultContent};
use sven_turn::assembly::{ContextPolicy, PolicyConfig};

struct Fixture {
    session: Arc<ChunkSession>,
    store: SharedChunkStore,
    root: tempfile::TempDir,
    store_dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store: SharedChunkStore = Arc::new(Mutex::new(
        ChunkStore::open(store_dir.path(), StoreLimits::default()).unwrap(),
    ));
    let probe: Arc<dyn SourceProbe + Send + Sync> = Arc::new(FsProbe::new(root.path()).unwrap());
    Fixture {
        session: Arc::new(ChunkSession::new(Arc::clone(&store), probe)),
        store,
        root,
        store_dir,
    }
}

fn assembler(f: &Fixture, budget: u32) -> ContextAssembler {
    ContextAssembler::new(
        ContextPolicy::new(PolicyConfig {
            budget_tokens: budget,
            keep_recent_rounds: 1,
            min_demote_tokens: 16,
        }),
        Arc::clone(&f.session),
    )
}

fn call(id: &str, name: &str, args: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        },
    }
}

fn history(rounds: usize) -> Vec<Message> {
    let mut m = vec![Message::user("work")];
    for r in 0..rounds {
        let id = format!("c{r}");
        m.push(call(&id, "shell", &format!(r#"{{"command":"step {r}"}}"#)));
        let out = (1..=200)
            .map(|n| format!("r{r} line {n} of the output"))
            .collect::<Vec<_>>()
            .join("\n");
        m.push(Message::tool_result(&id, out));
    }
    m
}

fn text(m: &Message) -> &str {
    match &m.content {
        MessageContent::ToolResult {
            content: ToolResultContent::Text(t),
            ..
        } => t,
        _ => "",
    }
}

#[test]
fn a_file_read_is_recorded_with_its_version_and_goes_stale_when_the_file_changes() {
    let f = fixture();
    fs::write(f.root.path().join("a.txt"), "v1").unwrap();
    f.session
        .record_result("read_file", &json!({"path": "a.txt"}), "L1:v1");
    let id = ChunkId::for_content(b"L1:v1");
    let probe = FsProbe::new(f.root.path()).unwrap();
    let store = f.store.lock().unwrap();
    assert_eq!(store.freshness(&id, &probe).unwrap(), Freshness::Fresh);
    drop(store);

    fs::write(f.root.path().join("a.txt"), "version two").unwrap();
    let store = f.store.lock().unwrap();
    assert!(matches!(
        store.freshness(&id, &probe).unwrap(),
        Freshness::Stale { .. }
    ));
}

#[test]
fn only_results_that_can_be_demoted_are_recorded() {
    let f = fixture();
    f.session
        .record_result("edit_file", &json!({"path": "a"}), "edited");
    f.session
        .record_result("shell", &json!({"command": "ls"}), "a b c");
    assert_eq!(f.store.lock().unwrap().len(), 1);
}

#[test]
fn every_demotion_is_backed_by_the_raw_text_in_the_store() {
    let f = fixture();
    let messages = history(5);
    let raw: Vec<String> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| text(m).to_string())
        .collect();
    let (view, record) = assembler(&f, 1_000).assemble(messages);
    assert!(record.demoted > 0 && record.tokens_saved > 0);
    assert_eq!(record.mode, "demote");
    let mut store = f.store.lock().unwrap();
    for (i, shown) in view.iter().filter(|m| m.role == Role::Tool).enumerate() {
        if text(shown) != raw[i] {
            let id = ChunkId::for_content(raw[i].as_bytes());
            assert_eq!(store.read(&id).unwrap(), raw[i]);
        }
    }
}

#[test]
fn a_demotion_is_named_in_the_record_once_and_counted_every_time() {
    let f = fixture();
    let mut a = assembler(&f, 1_000);
    let (_, first) = a.assemble(history(5));
    a.mark_sent(&first);
    let (_, second) = a.assemble(history(5));
    assert!(!first.new_demotions.is_empty());
    assert!(second.new_demotions.is_empty());
    assert_eq!(first.demoted, second.demoted);
    let d = &first.new_demotions[0];
    assert!(d.chunk.starts_with("c-") && d.digest.len() == 64 && d.tokens_saved > 0);
    assert_eq!((d.from.as_str(), d.tool.as_str()), ("full", "shell"));
}

#[test]
fn a_history_within_the_budget_is_untouched_and_records_no_demotions() {
    let f = fixture();
    let messages = history(3);
    let before = serde_json::to_string(&messages).unwrap();
    let (view, record) = assembler(&f, 1_000_000).assemble(messages);
    assert_eq!(serde_json::to_string(&view).unwrap(), before);
    assert_eq!((record.demoted, record.tokens_saved), (0, 0));
    assert_eq!(f.store.lock().unwrap().len(), 0);
}

#[test]
fn a_pinned_chunk_in_the_store_is_never_demoted() {
    let f = fixture();
    let messages = history(5);
    let first = text(&messages[2]).to_string();
    let id = f
        .session
        .record_result("shell", &json!({"command": "step 0"}), &first)
        .unwrap();
    f.store.lock().unwrap().set_pinned(&id, true).unwrap();
    let (view, _) = assembler(&f, 1_000).assemble(messages);
    assert_eq!(text(&view[2]), first, "{id} stays full");
}

#[test]
fn a_result_that_cannot_be_stored_is_left_whole() {
    let f = fixture();
    // The payload directory becomes a file, so no payload can be written.
    let payloads = f.store_dir.path().join("payloads");
    fs::remove_dir_all(&payloads).unwrap();
    fs::write(&payloads, "not a directory").unwrap();
    let messages = history(5);
    let before = serde_json::to_string(&messages).unwrap();
    let (view, record) = assembler(&f, 1_000).assemble(messages);
    assert_eq!(serde_json::to_string(&view).unwrap(), before);
    assert_eq!(record.demoted, 0);
}
