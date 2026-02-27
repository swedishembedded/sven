// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Peer pairing flow — the SSH-key-fingerprint equivalent for P2P clients.
//!
//! # How it works
//!
//! 1. The new device generates an Ed25519 keypair (done automatically by
//!    libp2p on first start).
//! 2. The device displays a pairing URI and/or QR code:
//!    `sven-pair://<peer_id>/<relay_multiaddr>`
//! 3. The agent operator runs:
//!    `sven gateway pair "sven-pair://12D3KooW.../ip4/1.2.3.4/tcp/4001/p2p/..."`
//! 4. The CLI shows the PeerId and a short fingerprint for visual confirmation:
//!    `"Authorize peer 12D3KooW...? Fingerprint: AB:CD:EF:12 [y/N]"`
//! 5. On confirmation the PeerId is added to the allowlist.
//!
//! The pairing URI contains **only public information** — the PeerId and a
//! reachable relay address. The private key never leaves the device. Unlike
//! password or token-based pairing, there is no secret that can be intercepted
//! in transit.

use libp2p::{Multiaddr, PeerId};

/// A parsed `sven-pair://` URI.
#[derive(Debug, Clone)]
pub struct PairingUri {
    /// The device's libp2p PeerId (base58-encoded in the URI).
    pub peer_id: PeerId,
    /// The relay (or direct) multiaddr the device can be reached at.
    pub addr: Option<Multiaddr>,
}

impl PairingUri {
    /// Parse a `sven-pair://` URI.
    ///
    /// Accepted forms:
    /// - `sven-pair://<peer_id>`
    /// - `sven-pair://<peer_id>/<multiaddr>`
    pub fn parse(uri: &str) -> anyhow::Result<Self> {
        let rest = uri.strip_prefix("sven-pair://")
            .ok_or_else(|| anyhow::anyhow!("URI must start with sven-pair://"))?;

        let (peer_str, addr_str) = match rest.find('/') {
            Some(pos) => (&rest[..pos], Some(&rest[pos + 1..])),
            None => (rest, None),
        };

        let peer_id = peer_str.parse::<PeerId>()
            .map_err(|e| anyhow::anyhow!("invalid PeerId in pairing URI: {e}"))?;

        let addr = addr_str
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Multiaddr>())
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid multiaddr in pairing URI: {e}"))?;

        Ok(Self { peer_id, addr })
    }

    /// Encode as a `sven-pair://` URI.
    pub fn to_uri(&self) -> String {
        match &self.addr {
            Some(addr) => format!("sven-pair://{}/{}", self.peer_id.to_base58(), addr),
            None => format!("sven-pair://{}", self.peer_id.to_base58()),
        }
    }

    /// Return a short human-friendly fingerprint (first 4 bytes of the
    /// multihash, hex-formatted as `AA:BB:CC:DD`) for visual confirmation.
    ///
    /// This mirrors how SSH displays host key fingerprints.
    pub fn short_fingerprint(&self) -> String {
        let bytes = self.peer_id.to_bytes();
        // Take the first 4 bytes (skip the varint multihash prefix when
        // present; for Ed25519 keys the payload starts at byte 2).
        let start = bytes.len().saturating_sub(6);
        bytes[start..start + 4.min(bytes.len() - start)]
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
        let uri = format!("sven-pair://{}", peer.to_base58());
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
        assert!(parsed.addr.is_none());
    }

    #[test]
    fn round_trip_without_addr() {
        let peer = sample_peer();
        let original = PairingUri { peer_id: peer, addr: None };
        let uri = original.to_uri();
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
    }

    #[test]
    fn round_trip_with_addr() {
        let peer = sample_peer();
        let addr: Multiaddr = "/ip4/1.2.3.4/tcp/4001".parse().unwrap();
        let original = PairingUri { peer_id: peer, addr: Some(addr.clone()) };
        let uri = original.to_uri();
        let parsed = PairingUri::parse(&uri).unwrap();
        assert_eq!(parsed.peer_id, peer);
        assert_eq!(parsed.addr.unwrap(), addr);
    }

    #[test]
    fn parse_rejects_wrong_scheme() {
        assert!(PairingUri::parse("https://something").is_err());
        assert!(PairingUri::parse("not-a-uri").is_err());
    }

    #[test]
    fn short_fingerprint_is_colon_separated_hex() {
        let peer = sample_peer();
        let uri = PairingUri { peer_id: peer, addr: None };
        let fp = uri.short_fingerprint();
        // Should look like "AB:CD:EF:12" — colons between hex pairs
        assert!(fp.contains(':'), "fingerprint must contain colons: {fp}");
        for part in fp.split(':') {
            assert!(part.len() == 2, "each part must be 2 hex chars: {part}");
        }
    }
}
