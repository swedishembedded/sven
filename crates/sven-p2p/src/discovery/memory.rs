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
    relay_addrs: Vec<Multiaddr>,
    /// room → peer_id_string → relay_addr
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

impl DiscoveryProvider for InMemoryDiscovery {
    fn publish_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        let mut g = self.inner.lock().unwrap();
        g.relay_addrs = addrs.to_vec();
        Ok(())
    }

    fn fetch_relay_addrs(&self) -> Result<Vec<Multiaddr>, P2pError> {
        let g = self.inner.lock().unwrap();
        if g.relay_addrs.is_empty() {
            return Err(P2pError::NoRelayAddrs);
        }
        Ok(g.relay_addrs.clone())
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
                Some(PeerInfo { peer_id, relay_addr: addr.clone() })
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
