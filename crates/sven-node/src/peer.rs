// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lightweight peer connectivity for the `sven peer` CLI subcommands.
//!
//! Starts an ephemeral P2P node — no HTTP server, no TLS, no agent loop —
//! using the same persistent Ed25519 keypair as `sven node start` so this
//! machine's peer ID is stable across invocations.
//!
//! All functions in this module that require P2P start the swarm themselves;
//! there is no need to have `sven node start` running first.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use chrono::Utc;
use libp2p::PeerId;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    task::JoinHandle,
};
use uuid::Uuid;

use sven_p2p::{
    protocol::types::{AgentCard, ContentBlock, SessionMessageWire, SessionRole},
    store::{ConversationStore, MessageDirection},
    InMemoryDiscovery, P2pConfig, P2pEvent, P2pHandle, P2pNode,
};

use crate::{config::GatewayConfig, node::build_agent_card};

// ── Ephemeral P2P node ────────────────────────────────────────────────────────

/// Start an ephemeral P2P node.
///
/// Uses the same persistent keypair and peer configuration as `sven node start`
/// so that:
/// - This machine's peer ID is stable (remote peers keep us in their allowlist).
/// - The configured peer allowlist is respected (deny-all by default).
///
/// The node runs in a background task.  Drop the returned `JoinHandle` to let
/// it run forever, or `abort()` it to stop the swarm.
pub async fn connect(config: &GatewayConfig) -> anyhow::Result<(P2pHandle, JoinHandle<()>)> {
    let agent_card = build_agent_card(config);

    let listen: libp2p::Multiaddr = config
        .swarm
        .listen
        .parse()
        .context("invalid swarm.listen address")?;

    let keypair_path = config
        .swarm
        .keypair_path
        .clone()
        .or_else(default_keypair_path);

    let agent_peers: HashSet<PeerId> = config
        .swarm
        .peers
        .keys()
        .filter_map(|s| {
            s.parse::<PeerId>()
                .map_err(|e| tracing::warn!("invalid peer ID {s:?}: {e}"))
                .ok()
        })
        .collect();

    let store_path = Some(ConversationStore::default_dir());

    let p2p_config = P2pConfig {
        listen_addr: listen,
        rooms: config.swarm.rooms.clone(),
        agent_card,
        discovery: Arc::new(InMemoryDiscovery::default()),
        keypair_path,
        discovery_poll_interval: Duration::from_secs(10),
        agent_peers,
        store_path,
    };

    let node = P2pNode::new(p2p_config);
    let handle = node.handle();

    let join = tokio::spawn(async move {
        if let Err(e) = node.run().await {
            tracing::warn!("ephemeral P2P node exited: {e}");
        }
    });

    Ok((handle, join))
}

// ── sven peer list ────────────────────────────────────────────────────────────

/// Discover and print all connected agent peers.
///
/// Starts an ephemeral P2P node, waits `timeout_secs` for mDNS discovery and
/// peer connections, then prints the roster.
pub async fn list_agent_peers(config: &GatewayConfig, timeout_secs: u64) -> anyhow::Result<()> {
    let (handle, _node) = connect(config).await?;

    let local_id = wait_for_local_id(&handle).await;
    eprintln!("Local peer ID: {local_id}");
    eprintln!("Discovering peers ({timeout_secs}s)...");

    tokio::time::sleep(Duration::from_secs(timeout_secs)).await;

    let peers = handle.all_peers();
    if peers.is_empty() {
        println!("No agent peers found.");
        println!();
        println!("Make sure other agents are running and that their peer IDs are");
        println!("listed in your swarm.peers config (~/.config/sven/gateway.yaml).");
        println!();
        println!("Your peer ID: {local_id}");
        println!("(Give this to peers so they can add you to their allowlists.)");
    } else {
        println!("{} peer(s) found:\n", peers.len());
        for (peer_id, card) in &peers {
            println!("  \x1b[1m{}\x1b[0m", card.name);
            println!("    Peer ID:      {peer_id}");
            if !card.description.is_empty() {
                println!("    Description:  {}", card.description);
            }
            if !card.capabilities.is_empty() {
                println!("    Capabilities: {}", card.capabilities.join(", "));
            }
            println!();
        }
    }
    Ok(())
}

// ── sven peer chat ────────────────────────────────────────────────────────────

/// Open an interactive terminal chat session with a remote peer agent.
///
/// Starts an ephemeral P2P node, resolves the peer (by name or peer ID), and
/// enters a line-by-line chat loop:
///
/// ```text
/// Connected to backend-agent (12D3KooWAbc…)
///
/// You: review the auth module
/// ⏳  waiting for reply…
/// backend-agent: The auth module looks correct. However, the JWT expiry…
///
/// You: _
/// ```
///
/// Messages are stored in the local conversation store alongside those from
/// `sven node start` sessions — everything is in one place.
pub async fn chat(config: &GatewayConfig, peer_target: &str) -> anyhow::Result<()> {
    let (handle, _node) = connect(config).await?;

    eprintln!("Connecting…");

    // Resolve the target peer — wait up to 10s for it to appear in the roster.
    let (peer_id, peer_name) = wait_for_named_peer(&handle, peer_target, Duration::from_secs(10))
        .await
        .with_context(|| {
            format!(
                "could not find peer {peer_target:?} within 10 seconds\n\
                 Make sure the peer is running and both sides have each other in swarm.peers."
            )
        })?;

    // Print recent history so the conversation has context.
    let store = ConversationStore::new(ConversationStore::default_dir());
    let recent = store.load_context_after_break(&peer_id.to_base58(), Duration::from_secs(3600))?;

    println!();
    println!("\x1b[1mConnected to {} ({})\x1b[0m", peer_name, peer_id);
    if !recent.is_empty() {
        println!(
            "\x1b[2m--- recent history ({} messages) ---\x1b[0m",
            recent.len()
        );
        for r in &recent {
            let who = if r.direction == MessageDirection::Outbound {
                "\x1b[1;32mYou\x1b[0m"
            } else {
                &format!("\x1b[1;34m{peer_name}\x1b[0m")
            };
            let text = extract_text(&r.content);
            let ts = r.timestamp.format("%H:%M");
            println!("{who} \x1b[2m({ts})\x1b[0m: {text}");
        }
        println!("\x1b[2m--- end of history ---\x1b[0m");
    }
    println!("\x1b[2mType a message and press Enter. Ctrl+C to exit.\x1b[0m");
    println!();

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        // Print prompt.
        print!("\x1b[1;32mYou:\x1b[0m ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();

        let line = match lines.next_line().await? {
            Some(l) => l,
            None => break, // EOF / Ctrl-D
        };
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }

        // Determine sequence number.
        let seq = store.message_count(&peer_id.to_base58()).unwrap_or(0);

        let msg = SessionMessageWire {
            message_id: Uuid::new_v4(),
            seq,
            timestamp: Utc::now(),
            role: SessionRole::User,
            content: vec![ContentBlock::text(&text)],
            depth: 0, // human-originated CLI message — start of a fresh session chain
        };

        if let Err(e) = handle.send_session_message(peer_id, msg).await {
            eprintln!("\x1b[31mFailed to send: {e}\x1b[0m");
            continue;
        }

        // Show waiting indicator.
        print!("\x1b[2m⏳  waiting for reply…\x1b[0m\r");
        let _ = std::io::stdout().flush();

        match handle
            .wait_for_message(peer_id, Duration::from_secs(300))
            .await
        {
            Ok(record) => {
                let reply = extract_text(&record.content);
                // Clear the waiting line, then print the reply.
                println!("\x1b[2K\x1b[1;34m{peer_name}:\x1b[0m {reply}");
                println!();
            }
            Err(sven_p2p::P2pError::Timeout) => {
                println!("\x1b[2K\x1b[31mNo reply within 5 minutes.\x1b[0m");
                println!();
            }
            Err(e) => {
                eprintln!("\x1b[31mError waiting for reply: {e}\x1b[0m");
            }
        }
    }

    println!("\n\x1b[2mSession ended.\x1b[0m");
    Ok(())
}

// ── sven peer search ──────────────────────────────────────────────────────────

/// Grep-style regex search over the local conversation history.
///
/// Pass `peer = Some("name-or-id")` to search within one peer's history, or
/// `peer = None` with `all = true` to search across all peers.
pub fn search(peer: Option<&str>, pattern: &str, limit: usize) -> anyhow::Result<()> {
    let store = ConversationStore::new(ConversationStore::default_dir());

    // Resolve a human-readable name to the stored peer ID if needed.
    // For search we match the peer_id field in the records directly; the caller
    // may pass a base58 ID or a partial name (matched as a substring).
    let results = store.search(peer, pattern, limit)?;

    if results.is_empty() {
        println!("No matches for `{pattern}`.");
        if let Some(p) = peer {
            println!("(searched within peer `{p}`)");
        }
        return Ok(());
    }

    println!("{} match(es) for `{pattern}`:\n", results.len());
    for r in &results {
        let dir = if r.direction == MessageDirection::Outbound {
            "\x1b[32m→\x1b[0m"
        } else {
            "\x1b[34m←\x1b[0m"
        };
        let ts = r.timestamp.format("%Y-%m-%d %H:%M UTC");
        println!("{dir} {ts}  [{}]  seq={}", r.peer_id, r.seq);
        for block in &r.content {
            if let ContentBlock::Text { text } = block {
                for line in text.lines().take(6) {
                    println!("    {line}");
                }
                if text.lines().count() > 6 {
                    println!(
                        "    \x1b[2m… ({} more lines)\x1b[0m",
                        text.lines().count() - 6
                    );
                }
            }
        }
        println!();
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_keypair_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/sven/gateway/agent-keypair"))
}

/// Wait until `P2pHandle::local_peer_id_string()` is populated (set after the
/// keypair is loaded during node startup), then return it.
async fn wait_for_local_id(handle: &P2pHandle) -> String {
    for _ in 0..50 {
        let id = handle.local_peer_id_string();
        if !id.is_empty() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    "<unknown>".to_string()
}

/// Wait for a peer matching `target` (by name or peer ID prefix) to appear in
/// the roster, up to `timeout`.
///
/// Returns `(PeerId, display_name)`.
async fn wait_for_named_peer(
    handle: &P2pHandle,
    target: &str,
    timeout: Duration,
) -> anyhow::Result<(PeerId, String)> {
    // Subscribe to events so we wake up the moment a peer is discovered.
    let mut events = handle.subscribe_events();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        // Check roster first.
        if let Some((pid, card)) = find_in_roster(handle, target) {
            return Ok((pid, card.name));
        }

        // Wait for the next event or timeout.
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        let sleep = tokio::time::sleep(remaining);
        tokio::pin!(sleep);
        tokio::select! {
            result = events.recv() => {
                match result {
                    Ok(P2pEvent::PeerDiscovered { .. }) | Ok(P2pEvent::Connected { .. }) => {
                        // Re-check the roster on next iteration.
                    }
                    Err(_) => bail!("P2P event channel closed"),
                    _ => {}
                }
            }
            _ = &mut sleep => break,
        }
    }

    // One final check after the timeout loop.
    if let Some((pid, card)) = find_in_roster(handle, target) {
        return Ok((pid, card.name));
    }

    bail!("peer {target:?} not found");
}

/// Look up `target` in the current roster.  Matches base58 peer ID (exact or
/// prefix) or agent name (case-insensitive substring).
fn find_in_roster(handle: &P2pHandle, target: &str) -> Option<(PeerId, AgentCard)> {
    let target_lower = target.to_lowercase();
    handle.all_peers().into_iter().find(|(pid, card)| {
        let pid_str = pid.to_base58();
        pid_str == target
            || pid_str.starts_with(target)
            || card.name.to_lowercase() == target_lower
            || card.name.to_lowercase().contains(&target_lower)
    })
}

/// Extract all text content blocks into a single string.
fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
