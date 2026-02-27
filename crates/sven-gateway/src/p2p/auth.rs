// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Peer authorization: who is allowed to operate this agent?
//!
//! # Security model
//!
//! libp2p's Noise handshake gives us **authentication** for free — by the time
//! our application code sees a `PeerId`, the handshake has cryptographically
//! verified that the peer possesses the Ed25519 private key corresponding to
//! that identity. We only need to decide **authorization**: is this authenticated
//! peer in our allowlist?
//!
//! **Default: deny all.** The allowlist starts empty. Peers must be explicitly
//! authorized via `sven gateway pair` or by editing the YAML file.
//!
//! # Usage
//!
//! ```rust
//! # use sven_gateway::p2p::auth::{PeerAllowlist, PeerRole};
//! # use libp2p::{PeerId, identity::Keypair};
//! // Empty allowlist denies everyone.
//! let list = PeerAllowlist::default();
//! let peer = PeerId::from(Keypair::generate_ed25519().public());
//! assert!(list.authorize(&peer).is_err());
//! ```
//!
//! ```rust,no_run
//! # use sven_gateway::p2p::auth::PeerAllowlist;
//! # use std::path::Path;
//! // Load from disk; missing file → empty (secure) default.
//! let mut list = PeerAllowlist::load(Path::new("~/.config/sven/gateway/authorized_peers.yaml"))
//!     .unwrap_or_default();
//! ```
//!
//! # File format (`~/.config/sven/gateway/authorized_peers.yaml`)
//!
//! ```yaml
//! operators:
//!   # peer_id: human-readable label
//!   "12D3KooW...": "my-phone"
//!   "12D3KooW...": "work-laptop"
//!
//! observers:
//!   "12D3KooW...": "ci-runner"
//! ```

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Role a peer is authorized to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    /// Can send all `ControlCommand`s (full control).
    Operator,
    /// Can subscribe to output but cannot send input or approve tools.
    Observer,
}

/// On-disk format for the authorized peers file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AuthorizedPeersFile {
    /// peer_id (base58) → human label
    #[serde(default)]
    pub operators: HashMap<String, String>,
    /// peer_id (base58) → human label
    #[serde(default)]
    pub observers: HashMap<String, String>,
}

/// Runtime peer allowlist. Loaded from YAML; updated by the pairing flow.
#[derive(Debug, Default, Clone)]
pub struct PeerAllowlist {
    operators: HashMap<PeerId, String>,
    observers: HashMap<PeerId, String>,
    /// Path where updates are persisted. `None` → in-memory only.
    path: Option<PathBuf>,
}

impl PeerAllowlist {
    /// Load from a YAML file. Missing file → empty allowlist (secure default).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self { path: Some(path.to_path_buf()), ..Default::default() });
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: AuthorizedPeersFile = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;

        let parse_map = |m: HashMap<String, String>| -> anyhow::Result<HashMap<PeerId, String>> {
            m.into_iter().map(|(id_str, label)| {
                id_str.parse::<PeerId>()
                    .map(|id| (id, label))
                    .map_err(|e| anyhow::anyhow!("invalid PeerId {id_str:?}: {e}"))
            }).collect()
        };

        Ok(Self {
            operators: parse_map(file.operators)?,
            observers: parse_map(file.observers)?,
            path: Some(path.to_path_buf()),
        })
    }

    /// Authorize a connected peer. Returns the role, or `Err` if not allowed.
    ///
    /// Called immediately after the Noise handshake so unauthorized peers are
    /// dropped before any application data is processed.
    pub fn authorize(&self, peer: &PeerId) -> Result<PeerRole, NotAuthorized> {
        if self.operators.contains_key(peer) {
            return Ok(PeerRole::Operator);
        }
        if self.observers.contains_key(peer) {
            return Ok(PeerRole::Observer);
        }
        Err(NotAuthorized(*peer))
    }

    /// Add a peer as an operator and persist the change.
    pub fn add_operator(&mut self, peer: PeerId, label: String) -> anyhow::Result<()> {
        self.operators.insert(peer, label.clone());
        info!(peer = %peer, label, "added operator peer");
        self.persist()
    }

    /// Add a peer as an observer and persist the change.
    pub fn add_observer(&mut self, peer: PeerId, label: String) -> anyhow::Result<()> {
        self.observers.insert(peer, label.clone());
        info!(peer = %peer, label, "added observer peer");
        self.persist()
    }

    /// Remove a peer from both operator and observer lists.
    pub fn revoke(&mut self, peer: &PeerId) -> anyhow::Result<bool> {
        let was_operator = self.operators.remove(peer).is_some();
        let was_observer = self.observers.remove(peer).is_some();
        if was_operator || was_observer {
            self.persist()?;
        }
        Ok(was_operator || was_observer)
    }

    /// Number of authorized operators.
    pub fn operator_count(&self) -> usize { self.operators.len() }

    /// Number of authorized observers.
    pub fn observer_count(&self) -> usize { self.observers.len() }

    fn persist(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else { return Ok(()); };

        let file = AuthorizedPeersFile {
            operators: self.operators.iter()
                .map(|(id, label)| (id.to_base58(), label.clone()))
                .collect(),
            observers: self.observers.iter()
                .map(|(id, label)| (id.to_base58(), label.clone()))
                .collect(),
        };
        let yaml = serde_yaml::to_string(&file).context("serializing authorized peers")?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating dir {}", parent.display()))?;
        }

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true).create(true).truncate(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("writing {}", path.display()))?;
            f.write_all(yaml.as_bytes())?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(path, yaml.as_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
        }

        Ok(())
    }
}

/// Error returned when a peer is not in the allowlist.
#[derive(Debug, thiserror::Error)]
#[error("peer {0} is not authorized")]
pub struct NotAuthorized(pub PeerId);

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer() -> PeerId {
        let key = libp2p::identity::Keypair::generate_ed25519();
        PeerId::from(key.public())
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let list = PeerAllowlist::default();
        let peer = make_peer();
        assert!(list.authorize(&peer).is_err());
    }

    #[test]
    fn added_operator_is_authorized() {
        let mut list = PeerAllowlist::default();
        let peer = make_peer();
        list.operators.insert(peer, "test".to_string());
        assert_eq!(list.authorize(&peer).unwrap(), PeerRole::Operator);
    }

    #[test]
    fn added_observer_gets_observer_role() {
        let mut list = PeerAllowlist::default();
        let peer = make_peer();
        list.observers.insert(peer, "ci".to_string());
        assert_eq!(list.authorize(&peer).unwrap(), PeerRole::Observer);
    }

    #[test]
    fn operator_also_gets_operator_not_observer() {
        let mut list = PeerAllowlist::default();
        let peer = make_peer();
        list.operators.insert(peer, "op".to_string());
        // Operators should be recognized as Operator, not Observer.
        assert_eq!(list.authorize(&peer).unwrap(), PeerRole::Operator);
    }

    #[test]
    fn revoke_removes_peer() {
        let mut list = PeerAllowlist::default();
        let peer = make_peer();
        list.operators.insert(peer, "op".to_string());
        assert!(list.revoke(&peer).unwrap());
        assert!(list.authorize(&peer).is_err());
    }

    #[test]
    fn yaml_round_trip_preserves_peers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.yaml");

        let key = libp2p::identity::Keypair::generate_ed25519();
        let peer = PeerId::from(key.public());

        let mut list = PeerAllowlist { path: Some(path.clone()), ..Default::default() };
        list.add_operator(peer, "my-phone".to_string()).unwrap();

        let loaded = PeerAllowlist::load(&path).unwrap();
        assert_eq!(loaded.authorize(&peer).unwrap(), PeerRole::Operator);
    }
}
