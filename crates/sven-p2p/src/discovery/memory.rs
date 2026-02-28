//! In-memory `DiscoveryProvider` — zero dependencies, suitable for tests,
//! local demos, and any scenario where peers run in the same process or on a
//! network where no Git remote is needed.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use libp2p::{Multiaddr, PeerId};

use crate::error::P2pError;

use super::{DiscoveryProvider, PeerInfo};

#[derive(Debug, Default)]
struct Inner {
    /// relay peer_id (string) → list of that relay's listen addresses.
    ///
    /// Keyed by peer_id so each relay's addresses are isolated: publishing a
    /// new set replaces only that relay's entries, and deletion removes exactly
    /// the addresses that were registered for that relay.
    relay_addrs: HashMap<String, Vec<Multiaddr>>,
    /// room → peer_id_string → relay_circuit_addr
    peers: HashMap<String, HashMap<String, Multiaddr>>,
}

/// Thread-safe in-memory implementation of `DiscoveryProvider`.
///
/// Multiple clones share the same underlying `Arc<Mutex<…>>` so that two nodes
/// constructed in the same test process see each other's registrations.
#[derive(Debug, Clone, Default)]
pub struct InMemoryDiscovery {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryDiscovery {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Extract the peer_id from a multiaddr that contains a `/p2p/<peer-id>` component.
fn peer_id_from_addr(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(mh) => PeerId::from_multihash(mh.into()).ok(),
        _ => None,
    })
}

impl DiscoveryProvider for InMemoryDiscovery {
    fn publish_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        // All addresses in a single publish call share the same relay peer_id.
        let peer_id = addrs
            .iter()
            .find_map(peer_id_from_addr)
            .map(|p| p.to_string())
            .unwrap_or_default();

        let mut g = self.inner.lock().unwrap();
        g.relay_addrs.insert(peer_id, addrs.to_vec());
        Ok(())
    }

    fn fetch_relay_addrs(&self) -> Result<Vec<Multiaddr>, P2pError> {
        let g = self.inner.lock().unwrap();
        let addrs: Vec<Multiaddr> = g.relay_addrs.values().flatten().cloned().collect();
        if addrs.is_empty() {
            return Err(P2pError::NoRelayAddrs);
        }
        Ok(addrs)
    }

    fn delete_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        // Derive the peer_id from the addresses being removed (same as publish).
        let peer_id = addrs
            .iter()
            .find_map(peer_id_from_addr)
            .map(|p| p.to_string())
            .unwrap_or_default();

        let mut g = self.inner.lock().unwrap();
        g.relay_addrs.remove(&peer_id);
        Ok(())
    }

    fn publish_peer(
        &self,
        room: &str,
        peer_id: &PeerId,
        relay_addr: &Multiaddr,
    ) -> Result<(), P2pError> {
        let mut g = self.inner.lock().unwrap();
        g.peers
            .entry(room.to_owned())
            .or_default()
            .insert(peer_id.to_string(), relay_addr.clone());
        Ok(())
    }

    fn fetch_peers(&self, room: &str) -> Result<Vec<PeerInfo>, P2pError> {
        let g = self.inner.lock().unwrap();
        let Some(room_map) = g.peers.get(room) else {
            return Ok(vec![]);
        };
        let peers = room_map
            .iter()
            .filter_map(|(id_str, addr)| {
                let peer_id = id_str.parse::<PeerId>().ok()?;
                Some(PeerInfo {
                    peer_id,
                    relay_addr: addr.clone(),
                })
            })
            .collect();
        Ok(peers)
    }

    fn delete_peer(&self, room: &str, peer_id: &PeerId) -> Result<(), P2pError> {
        let mut g = self.inner.lock().unwrap();
        if let Some(room_map) = g.peers.get_mut(room) {
            room_map.remove(&peer_id.to_string());
        }
        Ok(())
    }
}
