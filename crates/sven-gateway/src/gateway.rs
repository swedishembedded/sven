// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Gateway startup — assembles all subsystems and starts them.
//!
//! # Startup sequence
//!
//! [`run`] performs these steps in order, then blocks on the HTTP server:
//!
//! 1. Construct a [`ControlService`] that owns the agent and routes commands.
//! 2. Load or generate the HTTP bearer token; print it **once** if new.
//! 3. Load the P2P peer allowlist (deny-all if the file doesn't exist yet).
//! 4. Start the [`P2pControlNode`] in a background task.
//! 5. Start Slack Socket Mode tasks (one per configured account).
//! 6. Start the Axum HTTPS server (blocks until shutdown).
//!
//! # Pairing flow
//!
//! ```text
//! 1.  New device starts → generates Ed25519 keypair on first run.
//! 2.  Device displays:  sven-pair://12D3KooW.../ip4/1.2.3.4/tcp/4001
//! 3.  Operator runs:    sven gateway pair "sven-pair://12D3KooW..."
//! 4.  CLI shows PeerId + short fingerprint, asks for confirmation.
//! 5.  On "y":           PeerId added to authorized_peers.yaml (0o600).
//! 6.  Next P2P connection from that device is accepted.
//! ```
//!
//! # Token management
//!
//! ```text
//! First start:           token generated → SHA-256 hash stored in token.yaml
//!                        raw token printed once (save it!)
//! Mobile app:            Authorization: Bearer <token>
//! Rotate:                sven gateway regenerate-token
//!                        old token immediately invalid
//! ```

use std::{path::PathBuf, sync::Arc};

use tokio::sync::Mutex;
use tracing::info;

use sven_core::Agent;

use crate::{
    config::{GatewayConfig, SlackMode},
    control::service::ControlService,
    crypto::token::StoredTokenFile,
    http::slack::{run_socket_mode, SlackWebhookState},
    p2p::{auth::PeerAllowlist, handler::P2pControlNode},
};

/// Start the gateway, consuming the agent.
///
/// Spawns the following tasks:
/// 1. `ControlService` — owns the agent, processes commands.
/// 2. `P2pControlNode` — accepts libp2p connections from native clients.
/// 3. HTTP server — accepts browser/WebSocket connections.
/// 4. Slack Socket Mode tasks — one per configured Socket Mode account.
///
/// Runs until Ctrl+C or SIGTERM.
pub async fn run(config: GatewayConfig, agent: Agent) -> anyhow::Result<()> {
    // ── ControlService ────────────────────────────────────────────────────────
    let (service, agent_handle) = ControlService::new(agent);
    tokio::spawn(service.run());

    // ── Token ─────────────────────────────────────────────────────────────────
    let token_path = config
        .http
        .token_file
        .clone()
        .unwrap_or_else(default_token_path);
    let token_hash = if token_path.exists() {
        StoredTokenFile::load(&token_path)?.token_hash
    } else {
        info!("generating new gateway bearer token");
        let raw = StoredTokenFile::generate_and_save(&token_path)?;
        info!("=======================================================");
        info!("Gateway bearer token (shown once — save it now!):");
        info!("  {}", raw.as_str());
        info!("=======================================================");
        StoredTokenFile::load(&token_path)?.token_hash
    };

    // ── P2P allowlist ─────────────────────────────────────────────────────────
    let peers_path = config
        .p2p
        .authorized_peers_file
        .clone()
        .unwrap_or_else(default_peers_path);
    let allowlist = PeerAllowlist::load(&peers_path).unwrap_or_default();
    let allowlist = Arc::new(Mutex::new(allowlist));

    if allowlist.lock().await.operator_count() == 0 {
        info!(
            "No authorized P2P operators yet. Run:\n  sven gateway pair <sven-pair://...>\nto authorize a device."
        );
    }

    // ── P2P control node ──────────────────────────────────────────────────────
    let listen_addr: libp2p::Multiaddr = config
        .p2p
        .listen
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid P2P listen address: {e}"))?;

    let p2p_node = P2pControlNode::new(
        listen_addr,
        config.p2p.keypair_path.as_ref(),
        allowlist,
        agent_handle.clone(),
        config.p2p.mdns,
    )
    .await?;

    tokio::spawn(p2p_node.run());

    // ── Slack ─────────────────────────────────────────────────────────────────
    let mut slack_http_states = Vec::new();

    for account in config.slack.accounts {
        match account.mode {
            SlackMode::Socket => {
                info!("starting Slack Socket Mode");
                let handle = agent_handle.clone();
                let acct = account.clone();
                tokio::spawn(run_socket_mode(acct, handle));
            }
            SlackMode::Http => {
                let Some(ref secret) = account.signing_secret else {
                    tracing::error!("Slack HTTP mode requires signing_secret");
                    continue;
                };
                slack_http_states.push(SlackWebhookState {
                    signing_secret: Arc::new(secret.as_bytes().to_vec()),
                    agent: agent_handle.clone(),
                });
            }
        }
    }

    // ── HTTP server (blocks until shutdown) ───────────────────────────────────
    info!(
        bind = %config.http.bind,
        tls = !config.http.insecure_dev_mode,
        "starting HTTP gateway",
    );

    crate::http::serve(&config.http, agent_handle, token_hash, slack_http_states).await?;

    Ok(())
}

// ── Pairing subcommand ────────────────────────────────────────────────────────

/// Add a peer to the operator allowlist via a `sven-pair://` URI.
///
/// Called by `sven gateway pair <uri>`.
pub async fn pair_peer(
    config: &GatewayConfig,
    uri: &str,
    label: Option<String>,
) -> anyhow::Result<()> {
    use crate::p2p::pairing::PairingUri;

    let pairing = PairingUri::parse(uri)?;
    let fp = pairing.short_fingerprint();

    println!("Peer ID:       {}", pairing.peer_id.to_base58());
    println!("Fingerprint:   {fp}");
    println!(
        "Address:       {}",
        pairing
            .addr
            .as_ref()
            .map(|a| a.to_string())
            .unwrap_or("-".into())
    );
    println!();

    let label = label.unwrap_or_else(|| format!("device-{}", &pairing.peer_id.to_base58()[..8]));

    print!("Authorize this peer as an operator? (label: {label}) [y/N] ");
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let line = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;

    if line.trim().eq_ignore_ascii_case("y") {
        let peers_path = config
            .p2p
            .authorized_peers_file
            .clone()
            .unwrap_or_else(default_peers_path);
        let mut allowlist = PeerAllowlist::load(&peers_path).unwrap_or_default();
        allowlist.add_operator(pairing.peer_id, label.clone())?;
        println!("✓ Peer authorized as operator: {label}");
    } else {
        println!("Pairing cancelled.");
    }

    Ok(())
}

/// Revoke an authorized peer by PeerId string.
pub async fn revoke_peer(config: &GatewayConfig, peer_id_str: &str) -> anyhow::Result<()> {
    let peer_id: libp2p::PeerId = peer_id_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid PeerId: {e}"))?;

    let peers_path = config
        .p2p
        .authorized_peers_file
        .clone()
        .unwrap_or_else(default_peers_path);
    let mut allowlist = PeerAllowlist::load(&peers_path).unwrap_or_default();

    if allowlist.revoke(&peer_id)? {
        println!("✓ Peer {peer_id_str} revoked");
    } else {
        println!("Peer {peer_id_str} was not in the allowlist");
    }

    Ok(())
}

/// Regenerate the HTTP bearer token, printing the new raw token once.
pub fn regenerate_token(config: &GatewayConfig) -> anyhow::Result<()> {
    let token_path = config
        .http
        .token_file
        .clone()
        .unwrap_or_else(default_token_path);
    let raw = StoredTokenFile::generate_and_save(&token_path)?;
    println!("New bearer token (save it now — it won't be shown again):");
    println!("  {}", raw.as_str());
    println!();
    println!("Usage:  Authorization: Bearer {}", raw.as_str());
    Ok(())
}

// ── Default paths ─────────────────────────────────────────────────────────────

pub fn default_token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/sven/gateway/token.yaml")
}

pub fn default_peers_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/sven/gateway/authorized_peers.yaml")
}
