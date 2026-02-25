use std::{path::PathBuf, sync::Arc, time::Duration};

use libp2p::Multiaddr;

use crate::{discovery::DiscoveryProvider, protocol::types::AgentCard};

/// Configuration for a full P2P client node (publish/dial agent mode).
pub struct P2pConfig {
    /// Local TCP listen address. Use `/ip4/0.0.0.0/tcp/0` for an OS-assigned port.
    pub listen_addr: Multiaddr,

    /// Rooms this node will join and announce itself into.
    pub rooms: Vec<String>,

    /// This agent's identity card — broadcast to peers on connection.
    pub agent_card: AgentCard,

    /// Provider used to publish and fetch peer addresses.
    pub discovery: Arc<dyn DiscoveryProvider>,

    /// Path to persist the libp2p keypair. `None` generates a fresh ephemeral key.
    pub keypair_path: Option<PathBuf>,

    /// How often to poll the discovery provider for new peers (dial mode).
    pub discovery_poll_interval: Duration,
}

impl P2pConfig {
    pub fn new(
        listen_addr: Multiaddr,
        rooms: Vec<String>,
        agent_card: AgentCard,
        discovery: Arc<dyn DiscoveryProvider>,
    ) -> Self {
        Self {
            listen_addr,
            rooms,
            agent_card,
            discovery,
            keypair_path: None,
            discovery_poll_interval: Duration::from_secs(5),
        }
    }
}

/// Configuration for a relay-server node.
pub struct RelayConfig {
    /// Listen address (should be a public IP in production).
    pub listen_addr: Multiaddr,

    /// Path where the relay's keypair is persisted so its PeerId stays stable.
    pub keypair_path: PathBuf,

    /// Discovery provider used to publish the relay's own addresses.
    pub discovery: Arc<dyn DiscoveryProvider>,
}
