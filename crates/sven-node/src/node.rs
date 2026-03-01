// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Gateway startup — assembles all subsystems and starts them.
//!
//! # Startup sequence
//!
//! [`run`] performs these steps in order:
//!
//! 1. Build the `AgentCard` from config (name, description, capabilities).
//! 2. Create the `P2pNode` for agent-to-agent task routing.
//! 3. Build the `Agent` with the full standard toolset **plus** P2P routing
//!    tools (`delegate_task`, `list_peers`) wired to the live `P2pHandle`.
//! 4. Construct a [`ControlService`] that owns the agent.
//! 5. Spawn the P2P task executor loop (handles inbound `TaskRequested` events).
//! 6. Spawn the `P2pNode` swarm event loop.
//! 7. Load or generate the HTTP bearer token.
//! 8. Load the P2P peer allowlist (deny-all if the file doesn't exist yet).
//! 9. Start the [`P2pControlNode`] (operator control channel).
//! 10. Start Slack Socket Mode tasks.
//! 11. Start the Axum HTTPS server (blocks until shutdown).
//!
//! # Agent-to-agent task routing
//!
//! ```text
//! Remote agent
//!     │  libp2p Noise, /sven-p2p/task/1.0.0
//!     ▼
//! P2pNode::on_task_request()
//!     │  stores ResponseChannel, emits P2pEvent::TaskRequested
//!     ▼
//! task_executor_loop()           (spawned in background)
//!     │  creates ControlService session, sends input to Agent
//!     │  collects all OutputComplete events
//!     ▼
//! P2pHandle::reply_to_task()     → P2pCommand::TaskReply
//!     │
//!     ▼
//! NodeState::on_command(TaskReply)
//!     │  looks up stored ResponseChannel, calls send_response(TaskResult)
//!     ▼
//! Remote agent receives TaskResult
//! ```
//!
//! # Pairing flow (operator ↔ gateway)
//!
//! ```text
//! 1.  New device starts → generates Ed25519 keypair on first run.
//! 2.  Device displays:  sven://12D3KooW.../ip4/1.2.3.4/tcp/4001
//! 3.  Operator runs:    sven node authorize "sven://12D3KooW..."
//! 4.  CLI shows PeerId + short fingerprint, asks for confirmation.
//! 5.  On "y":           PeerId added to authorized_peers.yaml (0o600).
//! 6.  Next P2P connection from that device is accepted.
//! ```

use std::{path::PathBuf, sync::Arc};

use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

use libp2p::{Multiaddr, PeerId};
use sven_p2p::{
    protocol::types::{AgentCard, ContentBlock, P2pResponse, TaskStatus},
    InMemoryDiscovery, P2pConfig, P2pEvent, P2pHandle, P2pNode,
};

use crate::{
    agent_builder::build_gateway_agent,
    config::{GatewayConfig, SlackMode},
    control::{
        protocol::{ControlCommand, ControlEvent, SessionState},
        service::{AgentHandle, ControlService},
    },
    crypto::token::StoredTokenFile,
    http::slack::{run_socket_mode, SlackWebhookState},
    p2p::{auth::PeerAllowlist, handler::P2pControlNode},
};

/// Start the gateway, assembling all subsystems.
///
/// This is the single entry point for `sven gateway start`.  It owns the full
/// lifecycle: agent construction, P2P node, HTTP server, Slack tasks.
pub async fn run(
    config: GatewayConfig,
    sven_config: Arc<sven_config::Config>,
) -> anyhow::Result<()> {
    // ── Agent card ────────────────────────────────────────────────────────────
    let agent_card = build_agent_card(&config);
    info!(
        name = %agent_card.name,
        "gateway agent identity: {}",
        agent_card.description
    );

    // ── Agent-to-agent P2P node ───────────────────────────────────────────────
    let agent_p2p_listen: Multiaddr = "/ip4/0.0.0.0/tcp/0"
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid agent P2P listen address: {e}"))?;

    let agent_keypair_path = config
        .p2p
        .agent_keypair_path
        .clone()
        .or_else(|| default_agent_keypair_path());

    let p2p_config = P2pConfig {
        listen_addr: agent_p2p_listen,
        rooms: config.p2p.rooms.clone(),
        agent_card: agent_card.clone(),
        discovery: Arc::new(InMemoryDiscovery::default()),
        keypair_path: agent_keypair_path,
        discovery_poll_interval: std::time::Duration::from_secs(30),
    };

    let p2p_node = P2pNode::new(p2p_config);
    let p2p_handle = p2p_node.handle();

    // ── Build the agent with P2P routing tools ────────────────────────────────
    let agent = build_gateway_agent(
        &sven_config,
        p2p_handle.clone(),
        agent_card.clone(),
        config.p2p.rooms.clone(),
    )
    .await?;

    // ── ControlService ────────────────────────────────────────────────────────
    let (service, agent_handle) = ControlService::new(agent);
    tokio::spawn(service.run());

    // ── Inbound task executor loop ────────────────────────────────────────────
    let p2p_event_rx = p2p_handle.subscribe_events();
    tokio::spawn(run_task_executor(
        p2p_event_rx,
        p2p_handle.clone(),
        agent_handle.clone(),
        agent_card.clone(),
    ));

    // ── Spawn the P2pNode swarm ───────────────────────────────────────────────
    let rooms = config.p2p.rooms.clone();
    tokio::spawn(async move {
        match p2p_node.run().await {
            Ok(()) => info!("agent P2P node stopped"),
            Err(e) => tracing::error!("agent P2P node error: {e}"),
        }
    });
    info!(rooms = ?rooms, "agent P2P node started (mDNS discovery active)");

    // ── Token ─────────────────────────────────────────────────────────────────
    let token_path = config
        .http
        .token_file
        .clone()
        .unwrap_or_else(default_token_path);
    let token_hash = if token_path.exists() {
        info!(
            token_file = %token_path.display(),
            "HTTP bearer token loaded (use SVEN_GATEWAY_TOKEN or --token to connect)",
        );
        StoredTokenFile::load(&token_path)?.token_hash
    } else {
        let raw = StoredTokenFile::generate_and_save(&token_path)?;
        info!("=======================================================");
        info!("HTTP bearer token (shown once — save it now!):");
        info!("  {}", raw.as_str());
        info!("  export SVEN_GATEWAY_TOKEN={}", raw.as_str());
        info!("=======================================================");
        StoredTokenFile::load(&token_path)?.token_hash
    };

    // ── P2P operator allowlist ────────────────────────────────────────────────
    let peers_path = config
        .p2p
        .authorized_peers_file
        .clone()
        .unwrap_or_else(default_peers_path);
    let allowlist = PeerAllowlist::load(&peers_path).unwrap_or_default();
    let allowlist = Arc::new(Mutex::new(allowlist));

    if allowlist.lock().await.operator_count() == 0 {
        info!(
            "No P2P operator devices paired yet (optional — for mobile/native clients).\n  \
             To authorize a device: sven node authorize <sven://...>"
        );
    }

    // ── P2P operator control node ─────────────────────────────────────────────
    let listen_addr: Multiaddr = config
        .p2p
        .listen
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid P2P listen address: {e}"))?;

    // The control node is for human operator devices that connect via an
    // explicit sven:// URI — they are never discovered via mDNS.  Enabling
    // mDNS here causes the control node and the agent-to-agent P2pNode (both
    // running in the same process) to cross-discover each other.  The agent
    // P2pNode then tries to send a task-protocol Announce to the control node,
    // which fails with "none of the requested protocols", flooding the log and
    // triggering an infinite retry loop.
    let p2p_control_node = P2pControlNode::new(
        listen_addr,
        config.p2p.keypair_path.as_ref(),
        allowlist,
        agent_handle.clone(),
        false, // mDNS disabled — operator devices pair explicitly via sven:// URI
    )
    .await?;

    tokio::spawn(p2p_control_node.run());

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

// ── Inbound task executor ─────────────────────────────────────────────────────

/// Listens for `P2pEvent::TaskRequested` events from the agent P2P node and
/// executes each task via the `ControlService`.
///
/// Each task is processed in its own spawned task so multiple inbound tasks
/// can be served concurrently.
async fn run_task_executor(
    mut event_rx: tokio::sync::broadcast::Receiver<P2pEvent>,
    p2p: P2pHandle,
    agent: AgentHandle,
    our_card: AgentCard,
) {
    loop {
        match event_rx.recv().await {
            Ok(P2pEvent::TaskRequested { id, from, request }) => {
                let p2p = p2p.clone();
                let agent = agent.clone();
                let card = our_card.clone();
                tokio::spawn(async move {
                    execute_inbound_task(id, from, request, p2p, agent, card).await;
                });
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("P2P task executor lagged {n} events — some tasks may be lost");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Execute one inbound task through the local agent and send back the result.
async fn execute_inbound_task(
    task_id: Uuid,
    from: PeerId,
    request: sven_p2p::protocol::types::TaskRequest,
    p2p: P2pHandle,
    agent: AgentHandle,
    our_card: AgentCard,
) {
    use std::time::Instant;
    let start = Instant::now();

    tracing::info!(
        task_id = %task_id,
        from = %from,
        description = %request.description,
        "executing inbound P2P task"
    );

    // Build a prompt from the task request.
    let mut prompt = format!(
        "You have received a task from a peer agent (peer ID: {from}).\n\n\
         **Task:** {}\n",
        request.description
    );
    for block in &request.payload {
        match block {
            ContentBlock::Text { text } => {
                prompt.push_str("\n\n**Context:**\n");
                prompt.push_str(text);
            }
            ContentBlock::Json { value } => {
                prompt.push_str("\n\n**Context (JSON):**\n```json\n");
                prompt.push_str(&serde_json::to_string_pretty(value).unwrap_or_default());
                prompt.push_str("\n```");
            }
            ContentBlock::Image { .. } => {
                prompt.push_str("\n\n[Image context received but not yet supported]");
            }
        }
    }

    // Create a dedicated control session for this task.
    let session_id = Uuid::new_v4();
    let mut event_rx = agent.subscribe();

    let fail = |reason: String| {
        let p2p = p2p.clone();
        let our_card = our_card.clone();
        let request_id = request.id;
        let duration_ms = start.elapsed().as_millis() as u64;
        async move {
            tracing::warn!(task_id = %task_id, "P2P task failed: {reason}");
            let _ = p2p
                .reply_to_task(
                    task_id,
                    P2pResponse::TaskResult(sven_p2p::protocol::types::TaskResponse {
                        request_id,
                        agent: our_card,
                        result: vec![ContentBlock::text(&reason)],
                        status: TaskStatus::Failed { reason },
                        duration_ms,
                    }),
                )
                .await;
        }
    };

    if let Err(e) = agent
        .send(ControlCommand::NewSession {
            id: session_id,
            mode: sven_config::AgentMode::Agent,
            working_dir: None,
        })
        .await
    {
        fail(format!("failed to create session: {e}")).await;
        return;
    }

    // Drain the initial Idle event so our subscription is aligned.
    drain_until_idle(&mut event_rx, session_id).await;

    if let Err(e) = agent
        .send(ControlCommand::SendInput {
            session_id,
            text: prompt,
        })
        .await
    {
        fail(format!("failed to send task input: {e}")).await;
        return;
    }

    // Collect the final agent response (last OutputComplete before Completed).
    let task_timeout = tokio::time::Duration::from_secs(900);
    let deadline = tokio::time::Instant::now() + task_timeout;

    let mut last_response = String::new();

    loop {
        match tokio::time::timeout_at(deadline, event_rx.recv()).await {
            Ok(Ok(ControlEvent::OutputComplete {
                session_id: sid,
                text,
                role,
            })) if sid == session_id && role == "assistant" => {
                last_response = text;
            }
            Ok(Ok(ControlEvent::SessionState {
                session_id: sid,
                state: SessionState::Completed,
            })) if sid == session_id => {
                break;
            }
            Ok(Ok(ControlEvent::AgentError {
                session_id: Some(sid),
                message,
            })) if sid == session_id => {
                fail(message).await;
                return;
            }
            Ok(Ok(ControlEvent::SessionState {
                session_id: sid,
                state: SessionState::Cancelled,
            })) if sid == session_id => {
                fail("task session was cancelled".to_string()).await;
                return;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Ok(_) => continue,
            Err(_) => {
                fail("task timed out after 15 minutes".to_string()).await;
                return;
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    tracing::info!(
        task_id = %task_id,
        duration_ms,
        "P2P task completed"
    );

    let _ = p2p
        .reply_to_task(
            task_id,
            P2pResponse::TaskResult(sven_p2p::protocol::types::TaskResponse {
                request_id: request.id,
                agent: our_card,
                result: vec![ContentBlock::text(last_response)],
                status: TaskStatus::Completed,
                duration_ms,
            }),
        )
        .await;
}

/// Drain events until we see `SessionState::Idle` for `session_id`,
/// or until the broadcast lags / closes.
async fn drain_until_idle(
    rx: &mut tokio::sync::broadcast::Receiver<ControlEvent>,
    session_id: Uuid,
) {
    let timeout = tokio::time::Duration::from_millis(500);
    let _ = tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(ControlEvent::SessionState {
                    session_id: sid,
                    state: SessionState::Idle,
                }) if sid == session_id => return,
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    })
    .await;
}

// ── Pairing subcommand ────────────────────────────────────────────────────────

/// Add a peer to the operator allowlist via a `sven://` pairing URI.
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
    use std::io::{BufRead, Write};
    std::io::stdout().flush()?;
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
    let peer_id: PeerId = peer_id_str
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

/// List authorized operator devices (from the allowlist file).
///
/// These are human operator devices (phones, laptops) paired with
/// `sven node pair`. This is NOT the same as the agent `list_peers` tool,
/// which shows other sven nodes available for task delegation.
pub fn list_peers(config: &GatewayConfig) -> anyhow::Result<()> {
    let peers_path = config
        .p2p
        .authorized_peers_file
        .clone()
        .unwrap_or_else(default_peers_path);

    let allowlist = PeerAllowlist::load(&peers_path).unwrap_or_default();
    let peers = allowlist.all_peers();

    if peers.is_empty() {
        println!("No authorized operator devices.");
        println!();
        println!("Authorize a device with:  sven node authorize \"sven://...\"");
        println!();
        println!("Note: to see connected agent peers for task delegation, use");
        println!("      the list_peers tool inside a running session.");
        return Ok(());
    }

    println!("{} authorized operator device(s):\n", peers.len());
    for (peer_id, entry) in &peers {
        println!("  {} — {} (role: {:?})", entry.label, peer_id, entry.role);
    }
    println!();
    println!("Note: these are human operator devices, not agent peers.");
    println!("      To delegate tasks between agents, both nodes must be running");
    println!("      `sven node start` and discover each other via mDNS or relay.");
    Ok(())
}

// ── Agent card builder ────────────────────────────────────────────────────────

/// Build an `AgentCard` from the gateway config, filling in defaults from the
/// system hostname if no explicit identity is configured.
pub fn build_agent_card(config: &GatewayConfig) -> AgentCard {
    let default_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "sven-agent".to_string());

    let name = config.p2p.agent.name.clone().unwrap_or(default_name);

    let description = config
        .p2p
        .agent
        .description
        .clone()
        .unwrap_or_else(|| "General-purpose sven agent".to_string());

    AgentCard {
        peer_id: String::new(), // filled in by P2pNode::run() with the real PeerId
        name,
        description,
        capabilities: config.p2p.agent.capabilities.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

// ── Gateway exec (send task to a running gateway) ─────────────────────────────

/// Send a single task to a running gateway, stream the response to stdout.
///
/// Loads the gateway's self-signed TLS cert from the cert dir and trusts it
/// explicitly — no system roots needed, no danger flags.  Pass `insecure =
/// true` to skip cert verification entirely (useful when `insecure_dev_mode`
/// is enabled or the cert dir is unavailable).
pub async fn exec_task(
    config: &GatewayConfig,
    url: &str,
    token: &str,
    task: &str,
    insecure: bool,
) -> anyhow::Result<()> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async_tls_with_config, Connector};
    use tungstenite::http::Request;

    use crate::control::protocol::{ControlCommand, ControlEvent, SessionState};
    use sven_config::AgentMode;

    // Build the TLS connector — trust only the gateway's own cert.
    let connector = {
        let mut builder = native_tls::TlsConnector::builder();
        if insecure {
            builder.danger_accept_invalid_certs(true);
        } else {
            let cert_dir = config
                .http
                .tls_cert_dir
                .clone()
                .unwrap_or_else(crate::http::tls::default_cert_dir);
            let cert_path = cert_dir.join("gateway-cert.pem");
            match std::fs::read(&cert_path) {
                Ok(pem) => match native_tls::Certificate::from_pem(&pem) {
                    Ok(cert) => {
                        builder
                            .disable_built_in_roots(true)
                            .add_root_certificate(cert)
                            // The cert CN is "sven-node", not "127.0.0.1".
                            // We still verify the cert itself — just not the hostname.
                            .danger_accept_invalid_hostnames(true);
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "could not parse TLS cert from {}: {e}\n\
                             Hint: run with --insecure for dev gateways.",
                            cert_path.display()
                        );
                    }
                },
                Err(_) => {
                    anyhow::bail!(
                        "TLS cert not found at {}.\n\
                         Either start the gateway first, or use --insecure.",
                        cert_path.display()
                    );
                }
            }
        }
        Connector::NativeTls(builder.build()?)
    };

    // Build the WebSocket request with the bearer token.
    let request = Request::builder()
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Host", "127.0.0.1")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_ws_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())?;

    let (mut ws, _) = connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(|e| anyhow::anyhow!("could not connect to gateway at {url}: {e}"))?;

    // Open a session and send the task.
    let session_id = uuid::Uuid::new_v4();
    let new_session = serde_json::to_string(&ControlCommand::NewSession {
        id: session_id,
        mode: AgentMode::Agent,
        working_dir: None,
    })?;
    ws.send(tungstenite::Message::Text(new_session)).await?;

    let send_input = serde_json::to_string(&ControlCommand::SendInput {
        session_id,
        text: task.to_string(),
    })?;
    ws.send(tungstenite::Message::Text(send_input)).await?;

    // Stream events until the session completes.
    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| anyhow::anyhow!("WebSocket error: {e}"))?;
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let event: ControlEvent = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(_) => continue, // ignore unparseable frames
        };

        match event {
            ControlEvent::OutputDelta { delta, .. } => {
                print!("{delta}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            ControlEvent::OutputComplete { .. } => {
                println!();
            }
            ControlEvent::ToolCall { tool_name, .. } => {
                eprintln!("[tool: {tool_name}]");
            }
            ControlEvent::ToolNeedsApproval {
                tool_name,
                call_id,
                session_id,
                ..
            } => {
                // Auto-approve in exec mode — the user invoked the command
                // explicitly, so they implicitly approve all tools.
                let approve = serde_json::to_string(&ControlCommand::ApproveTool {
                    session_id,
                    call_id,
                })?;
                ws.send(tungstenite::Message::Text(approve)).await?;
                eprintln!("[auto-approved: {tool_name}]");
            }
            ControlEvent::SessionState { state, .. } => match state {
                SessionState::Completed | SessionState::Cancelled => break,
                _ => {}
            },
            ControlEvent::GatewayError { message, .. } => {
                anyhow::bail!("gateway error: {message}");
            }
            ControlEvent::AgentError { message, .. } => {
                eprintln!("agent error: {message}");
            }
            _ => {}
        }
    }

    Ok(())
}

fn generate_ws_key() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 16];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
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

fn default_agent_keypair_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/sven/gateway/agent-keypair"))
}
