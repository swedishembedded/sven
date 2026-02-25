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
/// # Key format
/// Keys are stored as protobuf-encoded `Keypair` (libp2p standard).  Older
/// versions of the relay demo used a raw 32-byte ed25519 secret key — those
/// files are **not** silently upgraded; a clear error is returned instead so
/// the operator can explicitly delete the stale file and let the relay
/// generate a fresh identity that it can publish to git.
///
/// Silently rotating the relay identity (the old behaviour) is dangerous: it
/// changes the PeerId without updating git, causing every connecting client to
/// fail with a `WrongPeerId` error until someone notices.
pub fn load_or_create_keypair(path: &Path) -> Result<identity::Keypair, P2pError> {
    if path.exists() {
        let raw = fs::read(path).map_err(|e| P2pError::Keypair(e.to_string()))?;

        // Try the current protobuf format first.
        if let Ok(key) = identity::Keypair::from_protobuf_encoding(&raw) {
            return Ok(key);
        }

        // Try the legacy raw-bytes ed25519 format used by the old relay demo
        // (32-byte secret scalar written with `secret.as_ref()`).
        if raw.len() == 32 {
            if let Ok(secret) = identity::ed25519::SecretKey::try_from_bytes(&mut raw.clone()) {
                let key = identity::Keypair::from(identity::ed25519::Keypair::from(secret));
                tracing::info!(
                    "Loaded legacy raw-ed25519 keypair from {}; migrating to protobuf format",
                    path.display()
                );
                let encoded = key
                    .to_protobuf_encoding()
                    .map_err(|e| P2pError::Keypair(e.to_string()))?;
                fs::write(path, &encoded).map_err(|e| P2pError::Keypair(e.to_string()))?;
                return Ok(key);
            }
        }

        // Unknown / corrupt format — refuse to silently rotate the identity.
        return Err(P2pError::Keypair(format!(
            "Keypair file '{}' ({} bytes) could not be decoded as protobuf or legacy \
             raw-ed25519 format. Delete the file to generate a new identity, then \
             restart the relay so it can publish the new PeerId to git.",
            path.display(),
            raw.len()
        )));
    }

    let key = identity::Keypair::generate_ed25519();
    let raw = key
        .to_protobuf_encoding()
        .map_err(|e| P2pError::Keypair(e.to_string()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| P2pError::Keypair(e.to_string()))?;
    }
    fs::write(path, &raw).map_err(|e| P2pError::Keypair(e.to_string()))?;
    tracing::info!("Generated new keypair at {}", path.display());
    Ok(key)
}
