// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agent runtimes that keep
// working when the parts around them fail for its clients. If your team needs
// expertise in fault-tolerant agent infrastructure then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The MCP manager never waits on whoever watches it: a surface that holds
//! the event receiver and never reads it cannot stall a connection, and
//! loses at most notices, never the news that the tools changed.

use std::collections::HashMap;
use std::time::Duration;

use sven_mcp_client::{McpManager, TokenStore};
use tokio::sync::mpsc;

#[tokio::test]
async fn a_watcher_that_never_reads_cannot_stall_the_manager() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, _unread) = mpsc::channel(1);
    let manager = McpManager::with_token_store(
        HashMap::new(),
        tx,
        false,
        TokenStore::new(dir.path().join("mcp-oauth.json")),
    );
    let mut changes = manager.tool_changes();
    for name in ["a", "b", "c"] {
        tokio::time::timeout(Duration::from_secs(5), manager.disconnect(name))
            .await
            .expect("the manager waited on a watcher that never reads");
    }
    assert!(changes.has_changed().unwrap());
    assert_eq!(
        *changes.borrow_and_update(),
        3,
        "each change is counted though its notice was dropped"
    );
}
