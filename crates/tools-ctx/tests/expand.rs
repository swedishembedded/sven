// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Expanding a reference returns the stored raw text, says when it is stale
//! and refuses anything that is not a chunk id.

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::json;
use sven_chunks::{
    ChunkDraft, ChunkKind, ChunkStore, FsProbe, Lifetime, Provenance, Sensitivity, SourceProbe,
    StoreLimits,
};
use sven_tool_api::tool::Tool;
use sven_tool_api::tool::ToolCall;
use sven_tools_ctx::{ChunkExpandTool, ChunkExpander};
use sven_vocab::AgentMode;

struct Fixture {
    expander: ChunkExpander,
    store: Arc<Mutex<ChunkStore>>,
    root: tempfile::TempDir,
    _store_dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(
        ChunkStore::open(store_dir.path(), StoreLimits::default()).unwrap(),
    ));
    let probe: Arc<dyn SourceProbe + Send + Sync> = Arc::new(FsProbe::new(root.path()).unwrap());
    Fixture {
        expander: ChunkExpander::new(Arc::clone(&store), probe),
        store,
        root,
        _store_dir: store_dir,
    }
}

fn draft(
    kind: ChunkKind,
    path: Option<&str>,
    version: Option<sven_chunks::SourceVersion>,
) -> ChunkDraft {
    ChunkDraft {
        kind,
        provenance: Provenance {
            source_tool: "read_file".into(),
            args_digest: String::new(),
            path: path.map(str::to_string),
            source_version: version,
            turn: 1,
        },
        priority: 0,
        sensitivity: Sensitivity::Internal,
        lifetime: Lifetime::Session,
        pinned: false,
        failure: false,
    }
}

fn expand(f: &Fixture, args: serde_json::Value) -> sven_tool_api::tool::ToolOutput {
    f.expander.expand(&ToolCall {
        id: "c1".into(),
        name: "context".into(),
        args,
    })
}

fn record(f: &Fixture, text: &str, d: ChunkDraft) -> String {
    f.store
        .lock()
        .unwrap()
        .put(text, d)
        .unwrap()
        .chunk
        .id
        .to_string()
}

#[test]
fn the_raw_text_comes_back_exactly_behind_a_header() {
    let f = fixture();
    let id = record(
        &f,
        "one\ntwo\nthree\n",
        draft(ChunkKind::CommandOutput, None, None),
    );
    let out = expand(&f, json!({"action": "expand", "chunk": id}));
    assert!(!out.is_error, "{}", out.content);
    let (header, body) = out.content.split_once('\n').unwrap();
    assert!(header.contains(&id) && header.contains("all 3 lines"));
    assert_eq!(body, "one\ntwo\nthree\n");
}

#[test]
fn a_line_range_returns_only_those_lines() {
    let f = fixture();
    let id = record(
        &f,
        "a\nb\nc\nd\n",
        draft(ChunkKind::CommandOutput, None, None),
    );
    let out = expand(&f, json!({"chunk": id, "first_line": 2, "last_line": 3}));
    assert!(out
        .content
        .starts_with(&format!("[chunk {id} | command_output | lines 2-3 of 4")));
    assert!(out.content.ends_with("\nb\nc\n"));
    let tail = expand(&f, json!({"chunk": id, "first_line": 4}));
    assert!(tail.content.ends_with("\nd\n"));
    let head = expand(&f, json!({"chunk": id, "last_line": 1}));
    assert!(head.content.ends_with("\na\n"));
}

#[test]
fn a_range_that_cannot_be_read_is_an_error() {
    let f = fixture();
    let id = record(&f, "a\nb\n", draft(ChunkKind::CommandOutput, None, None));
    for args in [
        json!({"chunk": id, "first_line": 0}),
        json!({"chunk": id, "first_line": 3, "last_line": 2}),
        json!({"chunk": id, "first_line": 9}),
    ] {
        assert!(expand(&f, args.clone()).is_error, "{args}");
    }
}

#[test]
fn an_unknown_or_malformed_chunk_id_is_refused() {
    let f = fixture();
    for chunk in ["c-0123456789abcdef", "../../etc/passwd", "c-../x", ""] {
        let out = expand(&f, json!({"chunk": chunk}));
        assert!(out.is_error, "{chunk:?}");
    }
    assert!(expand(&f, json!({})).is_error);
}

#[test]
fn an_expansion_of_a_changed_file_says_the_chunk_is_stale() {
    let f = fixture();
    let path = f.root.path().join("a.txt");
    fs::write(&path, "v1").unwrap();
    let probe = FsProbe::new(f.root.path()).unwrap();
    let version = probe.version("a.txt").unwrap();
    let id = record(&f, "v1", draft(ChunkKind::File, Some("a.txt"), version));

    let fresh = expand(&f, json!({"chunk": id}));
    assert!(fresh
        .content
        .lines()
        .next()
        .unwrap()
        .contains("unchanged since it was read"));

    fs::write(&path, "v2 is longer").unwrap();
    let stale = expand(&f, json!({"chunk": id}));
    assert!(!stale.is_error);
    assert!(
        stale.content.lines().next().unwrap().contains("STALE"),
        "{}",
        stale.content
    );
    assert!(
        stale.content.ends_with("\nv1"),
        "the old text is still returned"
    );
}

#[tokio::test]
async fn the_expansion_tool_is_the_context_tool_with_only_the_expand_action() {
    let f = fixture();
    let id = record(&f, "x\ny\n", draft(ChunkKind::CommandOutput, None, None));
    let tool = ChunkExpandTool::new(f.expander);
    assert_eq!(tool.name(), "context");
    assert!(tool.summary().len() <= 100, "{}", tool.summary());
    let schema = tool.parameters_schema();
    assert_eq!(schema["properties"]["action"]["enum"], json!(["expand"]));
    assert_eq!(schema["required"], json!(["action", "chunk"]));
    for key in ["chunk", "first_line", "last_line"] {
        assert!(schema["properties"].get(key).is_some(), "{key}");
    }
    for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
        assert!(tool.modes().contains(&mode));
    }
    assert_eq!(
        tool.call_capability(&json!({"action": "expand"})),
        sven_tool_api::ToolCapability::ReadFile
    );
    let out = tool
        .execute(&ToolCall {
            id: "c".into(),
            name: "context".into(),
            args: json!({"action": "expand", "chunk": id}),
        })
        .await;
    assert!(!out.is_error && out.content.ends_with("\nx\ny\n"));
    let other = tool
        .execute(&ToolCall {
            id: "c".into(),
            name: "context".into(),
            args: json!({"action": "open", "chunk": id}),
        })
        .await;
    assert!(other.is_error && other.content.contains("expand"));
}
