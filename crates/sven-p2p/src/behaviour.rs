//! libp2p `NetworkBehaviour` compositions.
//!
//! `P2pBehaviour`   — used by client (agent) nodes.
//! `RelayBehaviour` — used by the relay server.

use libp2p::{
    autonat, dcutr, identify, identity, ping, relay, request_response, swarm::NetworkBehaviour,
    PeerId,
};
use rand::rngs::OsRng;
use std::time::Duration;

use crate::protocol::codec::{P2pCodec, TASK_PROTO};

const APP_PROTO: &str = "/sven-p2p/1.0.0";

// ── Client / agent node behaviour ────────────────────────────────────────────

/// Combined behaviour for an agent node.
///
/// Includes:
/// - `relay::client` — to reserve slots on the relay server
/// - `dcutr`         — to attempt direct connections after relay (hole-punching)
/// - `identify`      — to exchange multiaddr and protocol lists with peers
/// - `autonat`       — to probe NAT type
/// - `ping`          — to keep idle connections alive
/// - `task`          — CBOR request/response for `AgentCard` announcements and task exchange
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "P2pBehaviourEvent")]
pub struct P2pBehaviour {
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub identify: identify::Behaviour,
    pub autonat: autonat::v2::client::Behaviour<OsRng>,
    pub ping: ping::Behaviour,
    pub task: request_response::Behaviour<P2pCodec>,
}

/// Unified event type produced by `P2pBehaviour`.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum P2pBehaviourEvent {
    Relay(relay::client::Event),
    Dcutr(dcutr::Event),
    Identify(identify::Event),
    Autonat(autonat::v2::client::Event),
    Ping(ping::Event),
    Task(
        request_response::Event<
            crate::protocol::types::P2pRequest,
            crate::protocol::types::P2pResponse,
        >,
    ),
}

impl From<relay::client::Event> for P2pBehaviourEvent {
    fn from(e: relay::client::Event) -> Self {
        P2pBehaviourEvent::Relay(e)
    }
}
impl From<dcutr::Event> for P2pBehaviourEvent {
    fn from(e: dcutr::Event) -> Self {
        P2pBehaviourEvent::Dcutr(e)
    }
}
impl From<identify::Event> for P2pBehaviourEvent {
    fn from(e: identify::Event) -> Self {
        P2pBehaviourEvent::Identify(e)
    }
}
impl From<autonat::v2::client::Event> for P2pBehaviourEvent {
    fn from(e: autonat::v2::client::Event) -> Self {
        P2pBehaviourEvent::Autonat(e)
    }
}
impl From<ping::Event> for P2pBehaviourEvent {
    fn from(e: ping::Event) -> Self {
        P2pBehaviourEvent::Ping(e)
    }
}
impl
    From<
        request_response::Event<
            crate::protocol::types::P2pRequest,
            crate::protocol::types::P2pResponse,
        >,
    > for P2pBehaviourEvent
{
    fn from(
        e: request_response::Event<
            crate::protocol::types::P2pRequest,
            crate::protocol::types::P2pResponse,
        >,
    ) -> Self {
        P2pBehaviourEvent::Task(e)
    }
}

impl P2pBehaviour {
    pub fn new(key: &identity::Keypair, relay_client: relay::client::Behaviour) -> Self {
        let local_peer_id = PeerId::from(key.public());
        Self {
            relay_client,
            dcutr: dcutr::Behaviour::new(local_peer_id),
            identify: identify::Behaviour::new(identify::Config::new(
                APP_PROTO.into(),
                key.public(),
            )),
            autonat: autonat::v2::client::Behaviour::new(OsRng, Default::default()),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(15))),
            task: request_response::Behaviour::with_codec(
                P2pCodec,
                [(TASK_PROTO, request_response::ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(Duration::from_secs(900)),
            ),
        }
    }
}

// ── Relay server behaviour ────────────────────────────────────────────────────

/// Combined behaviour for the relay server node.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "RelayBehaviourEvent")]
pub struct RelayBehaviour {
    pub relay: relay::Behaviour,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
}

#[derive(Debug)]
pub enum RelayBehaviourEvent {
    Relay(relay::Event),
    Identify(Box<identify::Event>),
    Ping(ping::Event),
}

impl From<relay::Event> for RelayBehaviourEvent {
    fn from(e: relay::Event) -> Self {
        RelayBehaviourEvent::Relay(e)
    }
}
impl From<identify::Event> for RelayBehaviourEvent {
    fn from(e: identify::Event) -> Self {
        RelayBehaviourEvent::Identify(Box::new(e))
    }
}
impl From<ping::Event> for RelayBehaviourEvent {
    fn from(e: ping::Event) -> Self {
        RelayBehaviourEvent::Ping(e)
    }
}

impl RelayBehaviour {
    pub fn new(key: &identity::Keypair) -> Self {
        let local_peer_id = PeerId::from(key.public());
        Self {
            relay: relay::Behaviour::new(local_peer_id, relay::Config::default()),
            identify: identify::Behaviour::new(identify::Config::new(
                APP_PROTO.into(),
                key.public(),
            )),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(15))),
        }
    }
}
