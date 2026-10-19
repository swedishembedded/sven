// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: with an embedder the `semantic_memory` tool recalls by meaning as well
//! as by word, and the trajectory names the generation that answered.
//!
//! The embedder is the local fixed model (`HashingEmbedder`): no server, no
//! weights, the same vectors on every machine. It sees spelling, not meaning,
//! which is enough to tell the two recall paths apart: a query word that is not
//! the stored word's prefix finds nothing by word and something by its letters.
//!
//! Swedish Embedded AB implements solutions for retrieval over private agent
//! memory for its clients. If your team needs expertise in semantic search that
//! keeps tenants apart then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use serde_json::{json, Value};
use sven_memory::{HashingEmbedder, SemanticMemoryTool};
use sven_memory_api::Embedder;
use sven_memory_sqlite::SqliteMemory;
use sven_tool_api::call_records::{scope, CallRecorder};
use sven_tool_api::tool::{Tool, ToolCall};

fn call(args: Value) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: "semantic_memory".into(),
        args,
    }
}

async fn run(tool: &SemanticMemoryTool, args: Value) -> String {
    let out = tool.execute(&call(args)).await;
    assert!(!out.is_error, "{}", out.content);
    out.content
}

#[tokio::test]
async fn a_misspelt_word_finds_by_letters_what_the_word_index_misses() {
    let store = Arc::new(SqliteMemory::open_in_memory().expect("a store"));
    let lexical = SemanticMemoryTool::personal(store.clone());
    let embedder = Arc::new(HashingEmbedder::default());
    let semantic = SemanticMemoryTool::personal(store.clone())
        .with_embedder(embedder.clone())
        .await
        .expect("the generation opens");

    run(
        &semantic,
        json!({"action": "remember", "content": "Deploying the service needs a rollout window"}),
    )
    .await;
    run(
        &semantic,
        json!({"action": "remember", "content": "The cat sat on the mat"}),
    )
    .await;

    // "deployment" is not a prefix of "deploying": the word index finds nothing.
    let by_word = run(&lexical, json!({"action": "recall", "query": "deployment"})).await;
    assert!(by_word.contains("No memories found"), "{by_word}");

    let recorder = CallRecorder::default();
    let by_letters = scope(
        recorder.clone(),
        run(
            &semantic,
            json!({"action": "recall", "query": "deployment"}),
        ),
    )
    .await;
    let deploy = by_letters
        .find("Deploying the service")
        .expect("found by letters");
    let cat = by_letters.find("The cat sat").unwrap_or(usize::MAX);
    assert!(deploy < cat, "the nearer note comes first: {by_letters}");

    // The trajectory says which index generation answered.
    let spec = embedder.spec();
    let record = recorder
        .memory_retrievals()
        .pop()
        .expect("a recorded recall");
    assert_eq!(
        record.generation_id,
        spec.generation_id("sven-segmenter-notes-1").to_string()
    );
}
