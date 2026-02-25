//! Peer-discovery abstraction.
//!
//! `DiscoveryProvider` is the single seam between the crate and whatever
//! storage backend is used (a Git repository, in-memory HashMap for tests, …).
//! The caller is responsible for constructing the concrete implementation and
//! wrapping it in an `Arc`.

pub mod memory;

#[cfg(feature = "git-discovery")]
pub mod git;

use libp2p::{Multiaddr, PeerId};

use crate::error::P2pError;

/// Information about a remote peer stored in the discovery backend.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    /// The peer's libp2p identity.
    pub peer_id: PeerId,
    /// The relay circuit address through which the peer is reachable.
    pub relay_addr: Multiaddr,
}

/// Backend-agnostic discovery interface.
///
/// All methods are synchronous (blocking is acceptable for git / in-process
/// memory); the event loop runs them on a blocking thread via `spawn_blocking`.
pub trait DiscoveryProvider: Send + Sync + 'static {
    // ── Relay ────────────────────────────────────────────────────────────────

    /// Persist the relay server's listen addresses so clients can find it.
    ///
    /// Each address is stored under a key derived from the address itself (e.g.
    /// SHA-256 in the git backend), so concurrent calls from different relay
    /// servers never conflict and do not overwrite each other.
    fn publish_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError>;

    /// Retrieve all relay addresses published by any relay server.
    fn fetch_relay_addrs(&self) -> Result<Vec<Multiaddr>, P2pError>;

    /// Remove exactly the relay addresses that were previously published.
    ///
    /// The caller passes the same slice that was given to `publish_relay_addrs`
    /// so implementations can derive the exact storage keys without scanning.
    /// Other relays' addresses are never touched.
    fn delete_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError>;

    // ── Peers ────────────────────────────────────────────────────────────────

    /// Publish this peer's relay circuit address under `room`.
    fn publish_peer(
        &self,
        room: &str,
        peer_id: &PeerId,
        relay_addr: &Multiaddr,
    ) -> Result<(), P2pError>;

    /// Retrieve all peers currently registered under `room`.
    fn fetch_peers(&self, room: &str) -> Result<Vec<PeerInfo>, P2pError>;

    /// Remove this peer's registration from `room` (called on graceful exit).
    fn delete_peer(&self, room: &str, peer_id: &PeerId) -> Result<(), P2pError>;
}
