//! Transport construction and keypair management.

use std::{fs, path::Path};

use libp2p::{
    core::{muxing::StreamMuxerBox, upgrade},
    identity, noise,
    swarm::Config as SwarmConfig,
    tcp, yamux, PeerId, Transport,
};

use crate::error::P2pError;

/// Build a TCP transport with Noise encryption and Yamux multiplexing.
///
/// This is the standard transport used by both client nodes and the relay server.
pub fn build_transport(
    key: &identity::Keypair,
) -> Result<libp2p::core::transport::Boxed<(PeerId, StreamMuxerBox)>, P2pError> {
    let noise_config =
        noise::Config::new(key).map_err(|e| P2pError::Transport(e.to_string()))?;

    let transport = tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
        .upgrade(upgrade::Version::V1)
        .authenticate(noise_config)
        .multiplex(yamux::Config::default())
        .boxed();
    Ok(transport)
}

/// Default swarm configuration: 30 s idle connection timeout so that relay
/// reservations and DCUtR hole-punching have enough time to complete.
pub fn default_swarm_config() -> SwarmConfig {
    use std::time::Duration;
    SwarmConfig::with_tokio_executor()
        .with_idle_connection_timeout(Duration::from_secs(30))
}

/// Load a persisted `identity::Keypair` from `path`, or generate a new one and
/// write it to `path` in protobuf encoding.
///
/// If the file exists but cannot be decoded it is deleted and a fresh keypair
/// is generated — no legacy format support.
pub fn load_or_create_keypair(path: &Path) -> Result<identity::Keypair, P2pError> {
    if path.exists() {
        let raw = fs::read(path).map_err(|e| P2pError::Keypair(e.to_string()))?;
        if let Ok(key) = identity::Keypair::from_protobuf_encoding(&raw) {
            return Ok(key);
        }
        tracing::warn!(
            "Keypair at {} could not be decoded; deleting and regenerating",
            path.display()
        );
        fs::remove_file(path).map_err(|e| P2pError::Keypair(e.to_string()))?;
    }

    let key = identity::Keypair::generate_ed25519();
    let raw = key
        .to_protobuf_encoding()
        .map_err(|e| P2pError::Keypair(e.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| P2pError::Keypair(e.to_string()))?;
    }
    fs::write(path, &raw).map_err(|e| P2pError::Keypair(e.to_string()))?;
    Ok(key)
}
