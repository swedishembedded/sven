// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running untrusted tool
// servers beside an agent's secrets for its clients. If your team needs
// expertise in agent sandboxing then you can procure our services by
// sending an email to info@swedishembedded.com.
//! A stdio server's spawn restriction: applied to its command before the
//! process starts, and a restriction that cannot be applied refuses the
//! start, so a server meant to be confined never runs unconfined.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sven_mcp_client::{McpManager, McpServerConfig, McpTransport, SpawnRestriction, TokenStore};
use tokio::sync::mpsc;

fn manager(restrict: SpawnRestriction, dir: &std::path::Path) -> Arc<McpManager> {
    let (tx, _events) = mpsc::channel(64);
    let config = McpServerConfig {
        transport: McpTransport::Stdio {
            command: "true".into(),
            args: Vec::new(),
        },
        timeout_secs: 5,
        restrict_spawn: Some(restrict),
        ..McpServerConfig::default()
    };
    McpManager::with_token_store(
        [("confined".to_string(), config)].into_iter().collect(),
        tx,
        false,
        TokenStore::new(dir.join("mcp-oauth.json")),
    )
}

#[tokio::test]
async fn a_restriction_that_cannot_be_applied_refuses_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let refused = manager(
        SpawnRestriction(Arc::new(|_| Err("no landlock here".into()))),
        dir.path(),
    );
    let error = refused.connect("confined").await.unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("cannot be confined: no landlock here"),
        "{message}"
    );
}

#[tokio::test]
async fn a_restriction_is_applied_to_every_start_of_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let applied = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&applied);
    let restricted = manager(
        SpawnRestriction(Arc::new(move |command| {
            seen.fetch_add(1, Ordering::SeqCst);
            command.env("RESTRICTED", "1");
            Ok(())
        })),
        dir.path(),
    );
    // `true` is no MCP server: the start is refused, but only after the
    // restriction was applied to it (both attempts: discovery, then
    // initialize).
    assert!(restricted.connect("confined").await.is_err());
    assert!(applied.load(Ordering::SeqCst) >= 1);
}
