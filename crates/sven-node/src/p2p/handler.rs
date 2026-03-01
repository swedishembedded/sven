// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! P2P control node — accepts connections from authorized native clients
//! (mobile apps, remote CLI) and bridges them to the [`ControlService`].
//!
//! # Transport security
//!
//! All P2P connections use **Noise protocol** (XX pattern) over TCP with Yamux
//! multiplexing. This provides:
//! - **Mutual authentication**: both sides prove their Ed25519 identity.
//! - **Encryption**: ChaCha20-Poly1305.
//! - **Forward secrecy**: ephemeral DH keys per session.
//!
//! # Authorization model
//!
//! 1. **Authentication** is implicit: libp2p Noise verifies the peer's
//!    Ed25519 key. By the time application code sees a `PeerId`, the Noise
//!    handshake has already succeeded.
//! 2. **Authorization** is enforced per-request in [`P2pControlNode::handle_request`]:
//!    the `PeerId` is looked up in the `PeerAllowlist`. Unknown peers receive
//!    `{ok: false, error: "not authorized"}` and no commands are forwarded.
//! 3. **Role enforcement**: `Observer` peers may only call `Subscribe`,
//!    `Unsubscribe`, and `ListSessions`. Any other command returns an error.
//!
//! # Wire protocol
//!
//! Uses libp2p's `request_response` behaviour with protocol `/sven/control/1.0.0`.
//!
//! Each request/response is CBOR-encoded with a 4-byte big-endian length prefix:
//!
//! ```text
//! ┌────────────┬────────────────────────────────┐
//! │ u32 BE len │ CBOR payload                   │
//! └────────────┴────────────────────────────────┘
//! ```
//!
//! - **Request** (`ControlP2pRequest`): wraps a `ControlCommand`.
//! - **Response** (`ControlP2pResponse`): `{ok, error?, events[]}` — events
//!   are all broadcast events buffered since the operator's last request.
//!
//! # Event delivery model
//!
//! Because `request_response` is synchronous (one response per request), events
//! are delivered via a **poll model**: each response carries all events
//! buffered since the last request.  Operators should poll frequently for
//! low-latency streaming, or wait for a server-push protocol when
//! `libp2p-stream` stabilises in a future libp2p release.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt};
use libp2p::{
    identity::Keypair,
    noise,
    request_response::{self, ProtocolSupport},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

use crate::{
    control::{
        protocol::{ControlCommand, ControlEvent},
        service::AgentHandle,
    },
    p2p::auth::{PeerAllowlist, PeerRole},
};

// ── Wire types for the control request_response protocol ─────────────────────

const CONTROL_PROTO: StreamProtocol = StreamProtocol::new("/sven/control/1.0.0");

/// A request from an operator to the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlP2pRequest {
    pub command: ControlCommand,
}

/// A response from the gateway to an operator.
///
/// Carries buffered events that have accumulated since the last poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlP2pResponse {
    pub ok: bool,
    pub error: Option<String>,
    /// Events that have accumulated since the previous request.
    pub events: Vec<ControlEvent>,
}

// ── CBOR codec ────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct ControlCodec;

#[async_trait]
impl request_response::Codec for ControlCodec {
    type Protocol = StreamProtocol;
    type Request = ControlP2pRequest;
    type Response = ControlP2pResponse;

    async fn read_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_cbor_framed(io).await
    }

    async fn read_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_cbor_framed(io).await
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_cbor_framed(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        resp: Self::Response,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_cbor_framed(io, &resp).await
    }
}

/// Maximum frame size: 8 MiB.
const MAX_FRAME: u32 = 8 * 1024 * 1024;

async fn read_cbor_framed<T, D>(io: &mut T) -> std::io::Result<D>
where
    T: AsyncRead + Unpin + Send,
    D: for<'de> serde::Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    io.read_exact(&mut buf).await?;
    ciborium::from_reader(&buf[..])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

async fn write_cbor_framed<T, S>(io: &mut T, value: &S) -> std::io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: serde::Serialize,
{
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let len = payload.len() as u32;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(&payload).await?;
    io.close().await
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::protocol::ControlCommand;
    use uuid::Uuid;

    /// Helper: write a value to an in-memory buffer, then read it back.
    async fn round_trip<T>(value: T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        // Write
        let mut buf: Vec<u8> = Vec::new();
        write_cbor_framed(&mut buf, &value).await.unwrap();

        // Read
        let mut cursor = futures::io::Cursor::new(buf);
        read_cbor_framed(&mut cursor).await.unwrap()
    }

    #[tokio::test]
    async fn cbor_framing_round_trips_request() {
        let original = ControlP2pRequest {
            command: ControlCommand::ListSessions,
        };
        let recovered: ControlP2pRequest = round_trip(original).await;
        assert!(matches!(recovered.command, ControlCommand::ListSessions));
    }

    #[tokio::test]
    async fn cbor_framing_round_trips_send_input() {
        let session_id = Uuid::new_v4();
        let original = ControlP2pRequest {
            command: ControlCommand::SendInput {
                session_id,
                text: "hello from operator".to_string(),
            },
        };
        let recovered: ControlP2pRequest = round_trip(original).await;
        match recovered.command {
            ControlCommand::SendInput {
                text,
                session_id: sid,
            } => {
                assert_eq!(text, "hello from operator");
                assert_eq!(sid, session_id);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cbor_framing_round_trips_response_with_events() {
        use crate::control::protocol::{ControlEvent, SessionState};
        let session_id = Uuid::new_v4();
        let original = ControlP2pResponse {
            ok: true,
            error: None,
            events: vec![
                ControlEvent::OutputDelta {
                    session_id,
                    delta: "chunk".to_string(),
                    role: "assistant".to_string(),
                },
                ControlEvent::SessionState {
                    session_id,
                    state: SessionState::Running,
                },
            ],
        };

        let recovered: ControlP2pResponse = round_trip(original).await;
        assert!(recovered.ok);
        assert_eq!(recovered.events.len(), 2);

        match &recovered.events[0] {
            ControlEvent::OutputDelta { delta, role, .. } => {
                assert_eq!(delta, "chunk");
                assert_eq!(role, "assistant");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cbor_framing_round_trips_error_response() {
        let original = ControlP2pResponse {
            ok: false,
            error: Some("not authorized".to_string()),
            events: vec![],
        };
        let recovered: ControlP2pResponse = round_trip(original).await;
        assert!(!recovered.ok);
        assert_eq!(recovered.error.as_deref(), Some("not authorized"));
        assert!(recovered.events.is_empty());
    }

    #[tokio::test]
    async fn cbor_frame_rejects_oversized_payload() {
        // Build a fake oversized length prefix (MAX_FRAME + 1).
        let oversized_len = (MAX_FRAME + 1).to_be_bytes();
        let mut fake_stream: Vec<u8> = Vec::new();
        fake_stream.extend_from_slice(&oversized_len);
        fake_stream.extend(vec![0u8; 8]); // dummy payload

        let mut cursor = futures::io::Cursor::new(fake_stream);
        let result: std::io::Result<ControlP2pRequest> = read_cbor_framed(&mut cursor).await;

        assert!(result.is_err(), "oversized frame must be rejected");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn cbor_framing_length_prefix_is_big_endian_u32() {
        // Verify the wire format: [u32 BE length][CBOR payload]
        let value = ControlP2pRequest {
            command: ControlCommand::ListSessions,
        };
        let mut buf: Vec<u8> = Vec::new();
        write_cbor_framed(&mut buf, &value).await.unwrap();

        // First 4 bytes are the big-endian payload length.
        assert!(buf.len() >= 4, "frame must have a 4-byte length prefix");
        let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(len, buf.len() - 4, "length prefix must match payload size");
    }

    #[tokio::test]
    async fn cbor_framing_truncated_payload_returns_error() {
        let value = ControlP2pRequest {
            command: ControlCommand::ListSessions,
        };
        let mut buf: Vec<u8> = Vec::new();
        write_cbor_framed(&mut buf, &value).await.unwrap();

        // Truncate the payload (keep length prefix but shorten the body).
        buf.truncate(5); // just past the 4-byte prefix, incomplete payload

        let mut cursor = futures::io::Cursor::new(buf);
        let result: std::io::Result<ControlP2pRequest> = read_cbor_framed(&mut cursor).await;
        assert!(result.is_err(), "truncated frame must return an error");
    }
}

// ── libp2p behaviour composition ──────────────────────────────────────────────

/// Behaviour for the P2P operator control node.
///
/// mDNS is intentionally omitted.  Operator devices pair explicitly via a
/// `sven://` URI — they are never discovered automatically.  Including mDNS
/// here causes the control node and the agent-to-agent `P2pNode` (both running
/// in the same process) to cross-discover each other via multicast, producing
/// hundreds of spurious "discovered/expired" log lines and establishing
/// unwanted inbound connections from every local network interface.
#[derive(NetworkBehaviour)]
struct ControlBehaviour {
    relay_client: libp2p::relay::client::Behaviour,
    identify: libp2p::identify::Behaviour,
    ping: libp2p::ping::Behaviour,
    control: request_response::Behaviour<ControlCodec>,
}

// ── Per-peer event buffer ─────────────────────────────────────────────────────

/// Buffered events for a connected operator.
struct PeerBuffer {
    rx: broadcast::Receiver<ControlEvent>,
    pending: Vec<ControlEvent>,
}

impl PeerBuffer {
    fn new(rx: broadcast::Receiver<ControlEvent>) -> Self {
        Self {
            rx,
            pending: Vec::new(),
        }
    }

    /// Drain any events that arrived since the last poll.
    fn drain(&mut self) -> Vec<ControlEvent> {
        // Non-blocking drain of the broadcast channel.
        loop {
            match self.rx.try_recv() {
                Ok(ev) => self.pending.push(ev),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("P2P operator buffer lagged by {n} events");
                    break;
                }
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        std::mem::take(&mut self.pending)
    }
}

// ── Public node ───────────────────────────────────────────────────────────────

/// The P2P control node. Call [`P2pControlNode::run`] in a spawned task.
pub struct P2pControlNode {
    swarm: libp2p::Swarm<ControlBehaviour>,
    allowlist: Arc<Mutex<PeerAllowlist>>,
    agent: AgentHandle,
    peer_buffers: std::collections::HashMap<PeerId, PeerBuffer>,
}

impl P2pControlNode {
    pub async fn new(
        listen_addr: Multiaddr,
        keypair_path: Option<&PathBuf>,
        allowlist: Arc<Mutex<PeerAllowlist>>,
        agent: AgentHandle,
    ) -> anyhow::Result<Self> {
        let keypair = match keypair_path {
            Some(path) => sven_p2p::transport::load_or_create_keypair(path)?,
            None => Keypair::generate_ed25519(),
        };

        let local_peer_id = PeerId::from(keypair.public());
        info!(peer_id = %local_peer_id, "P2P control node identity");

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_relay_client(noise::Config::new, yamux::Config::default)?
            .with_behaviour(|key, relay_client| {
                let identify = libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                    "/sven-node/1.0.0".into(),
                    key.public(),
                ));
                let ping = libp2p::ping::Behaviour::new(
                    libp2p::ping::Config::new().with_interval(std::time::Duration::from_secs(30)),
                );
                let control = request_response::Behaviour::with_codec(
                    ControlCodec,
                    [(CONTROL_PROTO, ProtocolSupport::Full)],
                    request_response::Config::default(),
                );

                Ok(ControlBehaviour {
                    relay_client,
                    identify,
                    ping,
                    control,
                })
            })?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(std::time::Duration::from_secs(120))
            })
            .build();

        swarm.listen_on(listen_addr)?;

        Ok(Self {
            swarm,
            allowlist,
            agent,
            peer_buffers: std::collections::HashMap::new(),
        })
    }

    /// Run the P2P event loop. Returns when the swarm is shut down.
    pub async fn run(mut self) {
        loop {
            let Some(event) = self.swarm.next().await else {
                break;
            };
            self.handle_swarm_event(event).await;
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<ControlBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "P2P control node listening");
            }

            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                // Subscribe this peer to the event broadcast.
                debug!(%peer_id, "P2P connection established");
                let rx = self.agent.subscribe();
                self.peer_buffers.insert(peer_id, PeerBuffer::new(rx));
            }

            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                debug!(%peer_id, "P2P connection closed");
                self.peer_buffers.remove(&peer_id);
            }

            SwarmEvent::Behaviour(ControlBehaviourEvent::Control(
                request_response::Event::Message {
                    peer,
                    connection_id: _,
                    message:
                        request_response::Message::Request {
                            request,
                            channel,
                            request_id: _,
                        },
                },
            )) => {
                self.handle_request(peer, request, channel).await;
            }

            _ => {}
        }
    }

    async fn handle_request(
        &mut self,
        peer: PeerId,
        req: ControlP2pRequest,
        channel: request_response::ResponseChannel<ControlP2pResponse>,
    ) {
        // Authorization check after Noise handshake.
        let role = {
            let list = self.allowlist.lock().await;
            list.authorize(&peer)
        };

        let role = match role {
            Ok(r) => r,
            Err(_) => {
                warn!(%peer, "unauthorized P2P request — dropping");
                let resp = ControlP2pResponse {
                    ok: false,
                    error: Some("not authorized".to_string()),
                    events: vec![],
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .control
                    .send_response(channel, resp);
                return;
            }
        };

        // Observer role enforcement.
        if role == PeerRole::Observer {
            use ControlCommand::*;
            match &req.command {
                Subscribe { .. } | Unsubscribe { .. } | ListSessions => {}
                _ => {
                    let resp = ControlP2pResponse {
                        ok: false,
                        error: Some("observer role: command not permitted".to_string()),
                        events: vec![],
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .control
                        .send_response(channel, resp);
                    return;
                }
            }
        }

        // Drain buffered events for this peer.
        let events = self
            .peer_buffers
            .get_mut(&peer)
            .map(|b| b.drain())
            .unwrap_or_default();

        // Forward command to the ControlService.
        if let Err(e) = self.agent.send(req.command).await {
            let resp = ControlP2pResponse {
                ok: false,
                error: Some(format!("service error: {e}")),
                events,
            };
            let _ = self
                .swarm
                .behaviour_mut()
                .control
                .send_response(channel, resp);
            return;
        }

        let resp = ControlP2pResponse {
            ok: true,
            error: None,
            events,
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .control
            .send_response(channel, resp);
    }
}
