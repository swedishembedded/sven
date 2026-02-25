//! High-level P2P node for agent instances.
//!
//! Obtain a `P2pHandle` before calling `run()` so you can send commands and
//! subscribe to events while the node event-loop runs inside a spawned task.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use futures::{future, StreamExt};
use libp2p::{
    core::{muxing::StreamMuxerBox, upgrade},
    identify,
    multiaddr::Protocol,
    noise, relay, request_response,
    swarm::{dial_opts::DialOpts, Swarm, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Transport,
};
use tokio::{
    sync::{broadcast, mpsc},
    time::{interval_at, Instant, MissedTickBehavior},
};
use uuid::Uuid;

use crate::{
    behaviour::{P2pBehaviour, P2pBehaviourEvent},
    config::P2pConfig,
    error::P2pError,
    protocol::types::{AgentCard, LogEntry, P2pRequest, P2pResponse, TaskRequest, TaskResponse},
    transport::{default_swarm_config, load_or_create_keypair},
};

// ── Public event / command types ──────────────────────────────────────────────

/// Events emitted by the P2P node to the host application.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum P2pEvent {
    PeerDiscovered { room: String, peer_id: PeerId, card: AgentCard },
    PeerLeft { room: String, peer_id: PeerId },
    Connected { peer_id: PeerId, via_relay: bool },
    Disconnected { peer_id: PeerId },
    TaskRequested { id: Uuid, from: PeerId, request: TaskRequest },
    TaskResponseReceived { id: Uuid, from: PeerId, response: TaskResponse },
    Error(P2pError),
}

#[derive(Debug)]
pub(crate) enum P2pCommand {
    SendTask { peer: PeerId, request: TaskRequest },
    Announce,
    Shutdown,
}

/// Snapshot of peers known in a single room.
#[derive(Debug, Clone, Default)]
pub struct RoomState {
    pub room: String,
    pub peers: HashMap<PeerId, AgentCard>,
}

// ── Handle ────────────────────────────────────────────────────────────────────

/// Cheap-to-clone handle to the running `P2pNode`.
///
/// Stores `broadcast::Sender`s so `Clone` is derivable; call `subscribe_events`
/// / `subscribe_logs` to obtain fresh receivers.
#[derive(Clone, Debug)]
pub struct P2pHandle {
    cmd_tx: mpsc::Sender<P2pCommand>,
    event_tx: broadcast::Sender<P2pEvent>,
    log_tx: broadcast::Sender<LogEntry>,
    roster: Arc<Mutex<HashMap<String, RoomState>>>,
}

impl P2pHandle {
    /// Subscribe to events from the P2P node.
    pub fn subscribe_events(&self) -> broadcast::Receiver<P2pEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to internal P2P log entries (TUI-safe).
    pub fn subscribe_logs(&self) -> broadcast::Receiver<LogEntry> {
        self.log_tx.subscribe()
    }

    /// Current peers in `room`.
    pub fn room_peers(&self, room: &str) -> Vec<(PeerId, AgentCard)> {
        let r = self.roster.lock().unwrap();
        r.get(room)
            .map(|rs| rs.peers.iter().map(|(k, v)| (*k, v.clone())).collect())
            .unwrap_or_default()
    }

    pub async fn send_task(&self, peer: PeerId, request: TaskRequest) -> Result<(), P2pError> {
        self.cmd_tx
            .send(P2pCommand::SendTask { peer, request })
            .await
            .map_err(|_| P2pError::Shutdown)
    }

    pub async fn announce(&self) -> Result<(), P2pError> {
        self.cmd_tx.send(P2pCommand::Announce).await.map_err(|_| P2pError::Shutdown)
    }

    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(P2pCommand::Shutdown).await;
    }
}

// ── P2pNode ───────────────────────────────────────────────────────────────────

pub struct P2pNode {
    config: P2pConfig,
    event_tx: broadcast::Sender<P2pEvent>,
    log_tx: broadcast::Sender<LogEntry>,
    cmd_tx: mpsc::Sender<P2pCommand>,
    cmd_rx: mpsc::Receiver<P2pCommand>,
    roster: Arc<Mutex<HashMap<String, RoomState>>>,
}

impl P2pNode {
    pub fn new(config: P2pConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let (log_tx, _) = broadcast::channel(512);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let roster: Arc<Mutex<HashMap<String, RoomState>>> = Arc::new(Mutex::new({
            let mut m = HashMap::new();
            for room in &config.rooms {
                m.insert(room.clone(), RoomState { room: room.clone(), peers: HashMap::new() });
            }
            m
        }));
        Self { config, event_tx, log_tx, cmd_tx, cmd_rx, roster }
    }

    pub fn handle(&self) -> P2pHandle {
        P2pHandle {
            cmd_tx: self.cmd_tx.clone(),
            event_tx: self.event_tx.clone(),
            log_tx: self.log_tx.clone(),
            roster: Arc::clone(&self.roster),
        }
    }

    /// Run the full event loop. Blocks until a `Shutdown` command or Ctrl-C.
    pub async fn run(mut self) -> Result<(), P2pError> {
        let key = match &self.config.keypair_path {
            Some(p) => load_or_create_keypair(p)?,
            None => libp2p::identity::Keypair::generate_ed25519(),
        };
        let local_peer_id = PeerId::from(key.public());
        let mut agent_card = self.config.agent_card.clone();
        agent_card.peer_id = local_peer_id.to_string();
        tracing::info!("P2pNode starting peer_id={local_peer_id}");

        // Build combined TCP + relay transport.
        // relay::client::new returns (Transport, Behaviour).
        let (relay_transport, relay_client) = relay::client::new(local_peer_id);

        let tcp_t = tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
            .upgrade(upgrade::Version::V1)
            .authenticate(
                noise::Config::new(&key)
                    .map_err(|e| P2pError::Transport(e.to_string()))?,
            )
            .multiplex(yamux::Config::default())
            .map(|(p, m), _| (p, StreamMuxerBox::new(m)));

        let relay_t = relay_transport
            .upgrade(upgrade::Version::V1)
            .authenticate(
                noise::Config::new(&key)
                    .map_err(|e| P2pError::Transport(e.to_string()))?,
            )
            .multiplex(yamux::Config::default())
            .map(|(p, m), _| (p, StreamMuxerBox::new(m)));

        let transport = tcp_t
            .or_transport(relay_t)
            .map(|either, _| match either {
                future::Either::Left(v) => v,
                future::Either::Right(v) => v,
            })
            .boxed();

        let behaviour = P2pBehaviour::new(&key, relay_client);
        let mut swarm = Swarm::new(transport, behaviour, local_peer_id, default_swarm_config());
        swarm
            .listen_on(self.config.listen_addr.clone())
            .map_err(|e| P2pError::Transport(e.to_string()))?;

        // Fetch relay addresses.
        let disc = Arc::clone(&self.config.discovery);
        let relay_addrs = tokio::task::spawn_blocking(move || disc.fetch_relay_addrs())
            .await
            .map_err(|e| P2pError::Discovery(e.to_string()))??;

        let relay_peer_id = relay_addrs
            .iter()
            .find_map(peer_id_from_addr)
            .ok_or_else(|| P2pError::Discovery("relay addr has no /p2p component".into()))?;

        let transport_addrs: Vec<Multiaddr> = relay_addrs
            .iter()
            .map(|a| {
                let mut a = a.clone();
                if matches!(a.iter().last(), Some(Protocol::P2p(_))) {
                    a.pop();
                }
                a
            })
            .collect();

        swarm
            .dial(DialOpts::peer_id(relay_peer_id).addresses(transport_addrs).build())
            .map_err(|e| P2pError::Dial(e.to_string()))?;

        let discovery = Arc::clone(&self.config.discovery);
        let rooms = self.config.rooms.clone();
        let poll_interval = self.config.discovery_poll_interval;
        let event_tx = self.event_tx.clone();
        let _log_tx = self.log_tx.clone();
        let roster = Arc::clone(&self.roster);

        let mut published = false;
        let mut connected_relay_addr: Option<Multiaddr> = None;
        let mut dialed: HashSet<PeerId> = HashSet::new();
        let mut failed: HashSet<PeerId> = HashSet::new();
        let _announced_to: HashSet<PeerId> = HashSet::new();

        let mut poll = interval_at(Instant::now() + poll_interval, poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        macro_rules! emit {
            ($ev:expr) => { let _ = event_tx.send($ev); }
        }

        loop {
            tokio::select! {
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Listening on {address}");
                        }

                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            let via_relay = endpoint.is_relayed();
                            emit!(P2pEvent::Connected { peer_id, via_relay });

                            if peer_id == relay_peer_id && connected_relay_addr.is_none() {
                                let full = endpoint.get_remote_address().clone();
                                tracing::info!("Relay reachable at {full}");
                                connected_relay_addr = Some(full.clone());
                                let circuit = mk_circuit_addr(&full, local_peer_id);
                                if let Err(e) = swarm.listen_on(circuit) {
                                    tracing::warn!("relay listen_on failed: {e}");
                                }
                            } else if peer_id != relay_peer_id {
                                swarm.behaviour_mut().task.send_request(
                                    &peer_id,
                                    P2pRequest::Announce(agent_card.clone()),
                                );
                            }
                        }

                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            emit!(P2pEvent::Disconnected { peer_id });
                        }

                        SwarmEvent::ExternalAddrConfirmed { address } => {
                            let addr_str = address.to_string();
                            if addr_str.contains("p2p-circuit") && !published {
                                tracing::info!("Relay circuit confirmed: {address}");
                                let disc2 = Arc::clone(&discovery);
                                let rooms2 = rooms.clone();
                                let addr2 = address.clone();
                                let local = local_peer_id;
                                tokio::task::spawn_blocking(move || {
                                    for room in &rooms2 {
                                        if let Err(e) = disc2.publish_peer(room, &local, &addr2) {
                                            tracing::warn!("publish_peer failed: {e}");
                                        } else {
                                            tracing::info!("Published refs/peers/{room}/{local}");
                                        }
                                    }
                                });
                                published = true;

                                // Immediately fetch existing peers.
                                for room in rooms.clone() {
                                    let d = Arc::clone(&discovery);
                                    let r = room.clone();
                                    if let Ok(Ok(peers)) = tokio::task::spawn_blocking(move || d.fetch_peers(&r)).await {
                                        dial_new_peers(&peers, local_peer_id, relay_peer_id, &mut dialed, &failed, &mut swarm);
                                    }
                                }
                            }
                        }

                        SwarmEvent::Behaviour(P2pBehaviourEvent::Relay(
                            relay::client::Event::ReservationReqAccepted { relay_peer_id: rp, .. }
                        )) => {
                            tracing::info!("Relay reservation accepted by {rp}");
                            if !published {
                                if let Some(relay_full) = &connected_relay_addr {
                                    let circuit = mk_circuit_addr(relay_full, local_peer_id);
                                    let disc2 = Arc::clone(&discovery);
                                    let rooms2 = rooms.clone();
                                    let local = local_peer_id;
                                    tokio::task::spawn_blocking(move || {
                                        for room in &rooms2 {
                                            if let Err(e) = disc2.publish_peer(room, &local, &circuit) {
                                                tracing::warn!("fallback publish failed: {e}");
                                            }
                                        }
                                    });
                                    published = true;
                                }
                            }
                        }

                        SwarmEvent::Behaviour(P2pBehaviourEvent::Task(
                            request_response::Event::Message { peer, message, .. }
                        )) => match message {
                            request_response::Message::Request { request, channel, .. } => {
                                match request {
                                    P2pRequest::Announce(card) => {
                                        {
                                            let mut r = roster.lock().unwrap();
                                            for room in &rooms {
                                                r.entry(room.clone())
                                                    .or_insert_with(|| RoomState {
                                                        room: room.clone(),
                                                        peers: HashMap::new(),
                                                    })
                                                    .peers
                                                    .insert(peer, card.clone());
                                            }
                                        }
                                        for room in &rooms {
                                            emit!(P2pEvent::PeerDiscovered {
                                                room: room.clone(),
                                                peer_id: peer,
                                                card: card.clone(),
                                            });
                                        }
                                        let _ = swarm.behaviour_mut().task
                                            .send_response(channel, P2pResponse::Ack);
                                    }
                                    P2pRequest::Task(req) => {
                                        let id = req.id;
                                        emit!(P2pEvent::TaskRequested { id, from: peer, request: req });
                                        let _ = swarm.behaviour_mut().task
                                            .send_response(channel, P2pResponse::Ack);
                                    }
                                }
                            }
                            request_response::Message::Response { response, .. } => {
                                if let P2pResponse::TaskResult(resp) = response {
                                    let id = resp.request_id;
                                    emit!(P2pEvent::TaskResponseReceived {
                                        id,
                                        from: peer,
                                        response: resp,
                                    });
                                }
                            }
                        },

                        SwarmEvent::Behaviour(P2pBehaviourEvent::Identify(
                            identify::Event::Received { peer_id, info, .. }
                        )) => {
                            for addr in info.listen_addrs {
                                swarm.add_peer_address(peer_id, addr);
                            }
                        }

                        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                            tracing::debug!("Connection error to {peer_id:?}: {error}");
                            if let Some(pid) = peer_id {
                                if failed.insert(pid) {
                                    dialed.remove(&pid); // allow one retry
                                }
                            }
                        }

                        _ => {}
                    }
                }

                _ = poll.tick() => {
                    if published {
                        for room in rooms.clone() {
                            let d = Arc::clone(&discovery);
                            let r = room.clone();
                            if let Ok(Ok(peers)) = tokio::task::spawn_blocking(move || d.fetch_peers(&r)).await {
                                dial_new_peers(&peers, local_peer_id, relay_peer_id, &mut dialed, &failed, &mut swarm);
                            }
                        }
                    }
                }

                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        P2pCommand::SendTask { peer, request } => {
                            swarm.behaviour_mut().task.send_request(&peer, P2pRequest::Task(request));
                        }
                        P2pCommand::Announce => {
                            let connected: Vec<PeerId> = swarm.connected_peers().copied().collect();
                            for peer in connected {
                                if peer != relay_peer_id {
                                    swarm.behaviour_mut().task.send_request(
                                        &peer,
                                        P2pRequest::Announce(agent_card.clone()),
                                    );
                                }
                            }
                        }
                        P2pCommand::Shutdown => break,
                    }
                }

                _ = tokio::signal::ctrl_c() => { break; }
            }
        }

        // Cleanup.
        if published {
            let disc2 = Arc::clone(&discovery);
            let rooms2 = rooms.clone();
            let local = local_peer_id;
            tokio::task::spawn_blocking(move || {
                for room in &rooms2 {
                    if let Err(e) = disc2.delete_peer(room, &local) {
                        tracing::warn!("cleanup delete_peer: {e}");
                    }
                }
            })
            .await
            .ok();
        }

        tracing::info!("P2pNode shut down");
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn peer_id_from_addr(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(mh) => PeerId::from_multihash(mh.into()).ok(),
        _ => None,
    })
}

fn mk_circuit_addr(server_addr: &Multiaddr, target: PeerId) -> Multiaddr {
    let mut a = server_addr.clone();
    a.push(Protocol::P2pCircuit);
    a.push(Protocol::P2p(target.into()));
    a
}

fn dial_new_peers(
    peers: &[crate::discovery::PeerInfo],
    local_peer_id: PeerId,
    relay_peer_id: PeerId,
    dialed: &mut HashSet<PeerId>,
    failed: &HashSet<PeerId>,
    swarm: &mut Swarm<P2pBehaviour>,
) {
    for info in peers {
        if info.peer_id == local_peer_id { continue; }
        if failed.contains(&info.peer_id) { continue; }
        if peer_id_from_addr(&info.relay_addr).map_or(true, |r| r != relay_peer_id) { continue; }
        if dialed.insert(info.peer_id) {
            tracing::info!("Dialing {} via relay", info.peer_id);
            if let Err(e) = swarm.dial(info.relay_addr.clone()) {
                tracing::warn!("dial failed: {e}");
            }
        }
    }
}
