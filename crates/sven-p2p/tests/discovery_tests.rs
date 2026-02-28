//! Tests for `InMemoryDiscovery` — synchronous, zero networking.

use std::sync::Arc;

use libp2p::{identity, PeerId};
use sven_p2p::discovery::{memory::InMemoryDiscovery, DiscoveryProvider};

fn new_peer() -> PeerId {
    PeerId::from(identity::Keypair::generate_ed25519().public())
}

fn addr(port: u16) -> libp2p::Multiaddr {
    format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()
}

fn circuit_addr(relay: &PeerId, client: &PeerId) -> libp2p::Multiaddr {
    format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{client}")
        .parse()
        .unwrap()
}

// ── Relay addresses ───────────────────────────────────────────────────────────

#[test]
fn relay_addrs_publish_and_fetch() {
    let disc = InMemoryDiscovery::new();
    let addrs = vec![addr(4001), addr(4002)];
    disc.publish_relay_addrs(&addrs).unwrap();
    let fetched = disc.fetch_relay_addrs().unwrap();
    assert_eq!(fetched, addrs);
}

#[test]
fn relay_addrs_overwrite() {
    let disc = InMemoryDiscovery::new();
    disc.publish_relay_addrs(&[addr(4001)]).unwrap();
    disc.publish_relay_addrs(&[addr(5001)]).unwrap();
    let fetched = disc.fetch_relay_addrs().unwrap();
    assert_eq!(fetched, vec![addr(5001)]);
}

#[test]
fn relay_addrs_not_found_without_publish() {
    let disc = InMemoryDiscovery::new();
    let result = disc.fetch_relay_addrs();
    assert!(
        result.is_err(),
        "should return error when no addrs published"
    );
}

// ── Peer CRUD ─────────────────────────────────────────────────────────────────

#[test]
fn publish_and_fetch_peer() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();
    let a_addr = circuit_addr(&relay_pid, &peer_a);

    disc.publish_peer("room1", &peer_a, &a_addr).unwrap();

    let peers = disc.fetch_peers("room1").unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].peer_id, peer_a);
    assert_eq!(peers[0].relay_addr, a_addr);
}

#[test]
fn fetch_peers_empty_room() {
    let disc = InMemoryDiscovery::new();
    let peers = disc.fetch_peers("nonexistent").unwrap();
    assert!(peers.is_empty());
}

#[test]
fn delete_peer_removes_it() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();
    let a_addr = circuit_addr(&relay_pid, &peer_a);

    disc.publish_peer("room1", &peer_a, &a_addr).unwrap();
    disc.delete_peer("room1", &peer_a).unwrap();

    let peers = disc.fetch_peers("room1").unwrap();
    assert!(peers.is_empty(), "peer should be gone after deletion");
}

#[test]
fn delete_nonexistent_peer_is_noop() {
    let disc = InMemoryDiscovery::new();
    let peer = new_peer();
    // Must not panic or return error.
    disc.delete_peer("room1", &peer).unwrap();
}

#[test]
fn overwrite_peer_updates_addr() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();
    let old_addr = circuit_addr(&relay_pid, &peer_a);
    let new_relay = new_peer();
    let new_addr = circuit_addr(&new_relay, &peer_a);

    disc.publish_peer("room1", &peer_a, &old_addr).unwrap();
    disc.publish_peer("room1", &peer_a, &new_addr).unwrap();

    let peers = disc.fetch_peers("room1").unwrap();
    assert_eq!(peers.len(), 1, "overwrite should not create a duplicate");
    assert_eq!(peers[0].relay_addr, new_addr);
}

#[test]
fn multiple_peers_in_same_room() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();
    let peer_b = new_peer();

    disc.publish_peer("room1", &peer_a, &circuit_addr(&relay_pid, &peer_a))
        .unwrap();
    disc.publish_peer("room1", &peer_b, &circuit_addr(&relay_pid, &peer_b))
        .unwrap();

    let peers = disc.fetch_peers("room1").unwrap();
    assert_eq!(peers.len(), 2);
}

// ── Room isolation ────────────────────────────────────────────────────────────

#[test]
fn rooms_are_isolated() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();

    disc.publish_peer("room-a", &peer_a, &circuit_addr(&relay_pid, &peer_a))
        .unwrap();

    let in_a = disc.fetch_peers("room-a").unwrap();
    let in_b = disc.fetch_peers("room-b").unwrap();

    assert_eq!(in_a.len(), 1, "peer_a should be visible in room-a");
    assert!(in_b.is_empty(), "peer_a must NOT appear in room-b");
}

#[test]
fn delete_from_one_room_does_not_affect_another() {
    let disc = InMemoryDiscovery::new();
    let relay_pid = new_peer();
    let peer_a = new_peer();

    disc.publish_peer("room-a", &peer_a, &circuit_addr(&relay_pid, &peer_a))
        .unwrap();
    disc.publish_peer("room-b", &peer_a, &circuit_addr(&relay_pid, &peer_a))
        .unwrap();

    disc.delete_peer("room-a", &peer_a).unwrap();

    assert!(disc.fetch_peers("room-a").unwrap().is_empty());
    assert_eq!(disc.fetch_peers("room-b").unwrap().len(), 1);
}

// ── Shared Arc<InMemoryDiscovery> between two nodes ───────────────────────────

#[test]
fn shared_discovery_visible_across_clones() {
    let disc = Arc::new(InMemoryDiscovery::new());
    let disc2 = Arc::clone(&disc);

    let relay_pid = new_peer();
    let peer_a = new_peer();
    let a_addr = circuit_addr(&relay_pid, &peer_a);

    disc.publish_peer("shared-room", &peer_a, &a_addr).unwrap();

    let peers = disc2.fetch_peers("shared-room").unwrap();
    assert_eq!(
        peers.len(),
        1,
        "peer published via disc must be visible via disc2"
    );
    assert_eq!(peers[0].peer_id, peer_a);
}
