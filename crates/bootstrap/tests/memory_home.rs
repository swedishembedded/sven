// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: where a session keeps its semantic memory is an input of the session,
//! never something it looks up on its own. A session built without a home
//! opens no store and touches nothing outside what it was given; a session
//! given a home keeps its store below that home and nowhere else.
//!
//! Swedish Embedded AB implements solutions for keeping agent runtimes from
//! touching state they were not given for its clients. If your team needs
//! expertise in hermetic agent test suites then you can procure our services
//! by sending an email to info@swedishembedded.com.
#![cfg(feature = "memory")]

use std::path::Path;
use std::sync::Arc;

use sven_bootstrap::{Config, RuntimeBuilder, RuntimeContext};

/// Whether a default session built on `context` offers `semantic_memory`.
async fn offers_semantic_memory(context: RuntimeContext) -> bool {
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    let (runtime, handle, ..) = RuntimeBuilder::new(Arc::new(config), "chat")
        .with_runtime_context(context)
        .build()
        .await
        .expect("a runtime builds");
    let present = handle.tool_registry().get("semantic_memory").is_some();
    runtime.abort();
    present
}

fn tree_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .expect("a readable directory")
        .count()
        == 0
}

#[tokio::test]
async fn a_session_without_a_home_opens_no_memory_store() {
    assert!(
        !offers_semantic_memory(RuntimeContext::empty()).await,
        "no home was given, so there is nowhere to keep a store"
    );
}

#[tokio::test]
async fn the_store_is_kept_below_the_home_the_session_was_given() {
    let given = tempfile::tempdir().expect("a home directory");
    let bystander = tempfile::tempdir().expect("an unrelated directory");

    let context = RuntimeContext {
        memory_home: Some(given.path().to_path_buf()),
        ..RuntimeContext::empty()
    };

    assert!(offers_semantic_memory(context).await);
    assert!(given
        .path()
        .join(".config/sven/memory/memory.sqlite")
        .is_file());
    assert!(tree_is_empty(bystander.path()));
}
