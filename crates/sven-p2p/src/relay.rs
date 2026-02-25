//! Relay server logic.
//!
//! A relay node runs `relay::Behaviour`, listens on a public address, and
//! publishes that address to the discovery backend so clients can find it.
//! It does not handle application-level messages — it only forwards circuits.

use std::sync::Arc;

use futures::StreamExt;
use libp2p::{
    multiaddr::Protocol,
    swarm::{Swarm, SwarmEvent},
    Multiaddr, PeerId,
};

use crate::{
    behaviour::{RelayBehaviour, RelayBehaviourEvent},
    config::RelayConfig,
    error::P2pError,
    transport::{build_transport, default_swarm_config, load_or_create_keypair},
};

/// Run the relay server until Ctrl-C is received.
///
/// Publishes the listen address to `config.discovery` after the first
/// `NewListenAddr` event so clients can discover the relay automatically.
pub async fn run(config: RelayConfig) -> Result<(), P2pError> {
    let key = load_or_create_keypair(&config.keypair_path)?;
    let local_peer_id = PeerId::from(key.public());
    tracing::info!("Relay server peer_id={local_peer_id}");

    let transport = build_transport(&key)?;
    let behaviour = RelayBehaviour::new(&key);
    let mut swarm = Swarm::new(transport, behaviour, local_peer_id, default_swarm_config());

    swarm
        .listen_on(config.listen_addr.clone())
        .map_err(|e| P2pError::Transport(e.to_string()))?;

    let discovery = Arc::clone(&config.discovery);
    let mut server_addrs: Vec<Multiaddr> = Vec::new();

    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        // Append /p2p/<peer-id> so clients can verify identity.
                        let full = address.with(Protocol::P2p(local_peer_id.into()));
                        tracing::info!("Relay listening on {full}");
                        // Tell libp2p about our external address so the relay
                        // behaviour includes it in Reservation responses.
                        swarm.add_external_address(full.clone());
                        server_addrs.push(full);
                        let disc = Arc::clone(&discovery);
                        let addrs = server_addrs.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = disc.publish_relay_addrs(&addrs) {
                                tracing::warn!("publish_relay_addrs failed: {e}");
                            } else {
                                tracing::info!("Published {} relay addr(s)", addrs.len());
                            }
                        });
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        tracing::debug!("Relay: connected to {peer_id}");
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        tracing::debug!("Relay: disconnected from {peer_id}");
                    }
                    SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(e)) => {
                        tracing::debug!("Relay event: {e:?}");
                    }
                    SwarmEvent::Behaviour(RelayBehaviourEvent::Identify(_)) => {}
                    SwarmEvent::Behaviour(RelayBehaviourEvent::Ping(_)) => {}
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Relay server shutting down");
                break;
            }
        }
    }

    Ok(())
}
