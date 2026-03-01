// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Peer pairing flow — the SSH-key-fingerprint equivalent for P2P clients.
//!
//! # How it works
//!
//! 1. The new device generates an Ed25519 keypair (done automatically by
//!    libp2p on first start).
//! 2. The device displays a pairing URI and/or QR code:
//!    `sven://<peer_id>/<relay_multiaddr>`
//! 3. The node operator runs:
//!    `sven node authorize "sven://12D3KooW.../ip4/1.2.3.4/tcp/4001/p2p/..."`
//! 4. The CLI shows the PeerId and a short fingerprint for visual confirmation:
//!    `"Authorize peer 12D3KooW...? Fingerprint: AB:CD:EF:12 [y/N]"`
//! 5. On confirmation the PeerId is added to the allowlist.
//!
//! The pairing URI contains **only public information** — the PeerId and a
//! reachable relay address. The private key never leaves the device. Unlike
//! password or token-based pairing, there is no secret that can be intercepted
//! in transit.

use libp2p::{Multiaddr, PeerId};
use sha2::{Digest, Sha256};

/// A parsed `sven://` pairing URI.
#[derive(Debug, Clone)]
pub struct PairingUri {
    /// The device's libp2p PeerId (base58-encoded in the URI).
    pub peer_id: PeerId,
    /// The relay (or direct) multiaddr the device can be reached at.
    pub addr: Option<Multiaddr>,
}

impl PairingUri {
    /// Parse a `sven://` pairing URI.
    ///
    /// Accepted forms:
    /// - `sven://<peer_id>`
    /// - `sven://<peer_id>/<multiaddr>`
    pub fn parse(uri: &str) -> anyhow::Result<Self> {
        let rest = uri
            .strip_prefix("sven://")
            .ok_or_else(|| anyhow::anyhow!("URI must start with sven://"))?;

        let (peer_str, addr_str) = match rest.find('/') {
            Some(pos) => (&rest[..pos], Some(&rest[pos + 1..])),
            None => (rest, None),
        };

        let peer_id = peer_str
            .parse::<PeerId>()
            .map_err(|e| anyhow::anyhow!("invalid PeerId in pairing URI: {e}"))?;

        let addr = addr_str
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Multiaddr>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid multiaddr in pairing URI: {e}"))?;

        Ok(Self { peer_id, addr })
    }

    /// Encode as a `sven://` pairing URI.
    pub fn to_uri(&self) -> String {
        match &self.addr {
            Some(addr) => format!("sven://{}/{}", self.peer_id.to_base58(), addr),
            None => format!("sven://{}", self.peer_id.to_base58()),
        }
    }

    /// Return a human-verifiable fingerprint of this peer's identity for use
    /// during the pairing confirmation step.
    ///
    /// # Security properties
    ///
    /// The fingerprint is the **SHA-256 of the full PeerId multihash bytes**,
    /// displayed as 16 colon-separated hex pairs (128 bits visible out of 256
    /// bits of digest).  This matches the format used by OpenSSH for SHA-256
    /// fingerprints and provides:
    ///
    /// * **Collision resistance**: an attacker must invert SHA-256 over the
    ///   full 256-bit PeerId space — computationally infeasible.
    /// * **Preimage resistance**: knowing the fingerprint does not reveal the
    ///   private key or allow key derivation.
    ///
    /// The previous 4-byte (32-bit) fingerprint could be brute-forced in
    /// ~65 536 keypair generations (seconds on modern hardware); this
    /// 128-bit display requires ~2^64 attempts — decades even with dedicated
    /// ASICs.
    pub fn short_fingerprint(&self) -> String {
        let digest = Sha256::digest(self.peer_id.to_bytes());
        // Display the first 16 bytes (128 bits) as colon-separated hex pairs,
        // matching the SSH `SHA256:` fingerprint display convention.
        digest[..16]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

impl std::fmt::Display for PairingUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_uri())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_peer() -> PeerId {
        let key = libp2p::identity::Keypair::generate_ed25519();
        PeerId::from(key.public())
    }

    #[test]
    fn parse_uri_without_addr() {
        let peer = sample_peer();
        let uri = format!("sven://{}", peer.to_base58());
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
        assert!(parsed.addr.is_none());
    }

    #[test]
    fn round_trip_without_addr() {
        let peer = sample_peer();
        let original = PairingUri {
            peer_id: peer,
            addr: None,
        };
        let uri = original.to_uri();
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
    }

    #[test]
    fn round_trip_with_addr() {
        let peer = sample_peer();
        let addr: Multiaddr = "/ip4/1.2.3.4/tcp/4001".parse().unwrap();
        let original = PairingUri {
            peer_id: peer,
            addr: Some(addr.clone()),
        };
        let uri = original.to_uri();
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
        assert_eq!(parsed.addr.unwrap(), addr);
    }

    #[test]
    fn parse_rejects_wrong_scheme() {
        assert!(PairingUri::parse("https://something").is_err());
        assert!(PairingUri::parse("not-a-uri").is_err());
        assert!(PairingUri::parse("sven-pair://something").is_err());
    }

    #[test]
    fn short_fingerprint_is_colon_separated_hex() {
        let peer = sample_peer();
        let uri = PairingUri {
            peer_id: peer,
            addr: None,
        };
        let fp = uri.short_fingerprint();
        // Should look like "AB:CD:EF:…" — 16 colon-separated hex pairs (128 bits)
        assert!(fp.contains(':'), "fingerprint must contain colons: {fp}");
        let parts: Vec<&str> = fp.split(':').collect();
        assert_eq!(parts.len(), 16, "fingerprint must have 16 hex pairs: {fp}");
        for part in &parts {
            assert_eq!(part.len(), 2, "each part must be 2 hex chars: {part}");
        }
    }

    #[test]
    fn different_peers_have_different_fingerprints() {
        let peer1 = sample_peer();
        let peer2 = sample_peer();
        let fp1 = PairingUri {
            peer_id: peer1,
            addr: None,
        }
        .short_fingerprint();
        let fp2 = PairingUri {
            peer_id: peer2,
            addr: None,
        }
        .short_fingerprint();
        assert_ne!(
            fp1, fp2,
            "different peers must produce different fingerprints"
        );
    }
}
