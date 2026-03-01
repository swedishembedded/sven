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
    core::{muxing::StreamMuxerBox, upgrade, ConnectedPoint},
    dcutr, identify,
    multiaddr::Protocol,
    noise, relay, request_response,
    swarm::{dial_opts::DialOpts, ConnectionId, DialError, Swarm, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Transport,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::{interval_at, Duration, Instant, MissedTickBehavior},
};
use uuid::Uuid;

use crate::{
    behaviour::{P2pBehaviour, P2pBehaviourEvent},
    config::P2pConfig,
    discovery::{DiscoveryProvider, PeerInfo},
    error::P2pError,
    protocol::types::{AgentCard, LogEntry, P2pRequest, P2pResponse, TaskRequest, TaskResponse},
    transport::{default_swarm_config, load_or_create_keypair},
};

/// Alias for the channel half used to reply to an inbound task.
type TaskReplySender = oneshot::Sender<Result<TaskResponse, P2pError>>;

/// Convenience alias used throughout this module.
type NodeSwarm = Swarm<P2pBehaviour>;

/// How often to re-fetch relay addresses from the discovery backend and
/// reconnect to any relay that dropped or newly appeared.
const RELAY_POLL_SECS: u64 = 30;

// ── Public event / command types ──────────────────────────────────────────────

/// Events emitted by the P2P node to the host application.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum P2pEvent {
    PeerDiscovered {
        room: String,
        peer_id: PeerId,
        card: AgentCard,
    },
    PeerLeft {
        room: String,
        peer_id: PeerId,
    },
    Connected {
        peer_id: PeerId,
        via_relay: bool,
    },
    Disconnected {
        peer_id: PeerId,
    },
    /// Fired once per relay when the circuit reservation is confirmed and our
    /// circuit address has been published to discovery.  The node is now
    /// reachable by other peers through this relay.
    RelayCircuitEstablished {
        relay_peer_id: Option<PeerId>,
    },
    TaskRequested {
        id: Uuid,
        from: PeerId,
        request: TaskRequest,
    },
    TaskResponseReceived {
        id: Uuid,
        from: PeerId,
        response: TaskResponse,
    },
    Error(P2pError),
}

#[derive(Debug)]
pub(crate) enum P2pCommand {
    /// Send a task to a peer and await the `TaskResponse` via the provided channel.
    SendTask {
        peer: PeerId,
        request: TaskRequest,
        reply_tx: TaskReplySender,
    },
    /// Reply to an inbound task request (called by the task executor after it
    /// finishes running the agent).
    TaskReply {
        id: uuid::Uuid,
        response: P2pResponse,
    },
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

    /// Send a task to `peer` and wait for the `TaskResponse`.
    ///
    /// Returns when the remote agent sends back a `TaskResult`, or with
    /// `P2pError::Shutdown` if the local node shuts down before the reply
    /// arrives.  The caller is responsible for applying its own timeout.
    pub async fn send_task(
        &self,
        peer: PeerId,
        request: TaskRequest,
    ) -> Result<TaskResponse, P2pError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(P2pCommand::SendTask {
                peer,
                request,
                reply_tx,
            })
            .await
            .map_err(|_| P2pError::Shutdown)?;
        reply_rx.await.map_err(|_| P2pError::Shutdown)?
    }

    /// Reply to an inbound task request.  Called by the task executor in the
    /// gateway once the agent has finished processing the task.
    pub async fn reply_to_task(
        &self,
        id: uuid::Uuid,
        response: P2pResponse,
    ) -> Result<(), P2pError> {
        self.cmd_tx
            .send(P2pCommand::TaskReply { id, response })
            .await
            .map_err(|_| P2pError::Shutdown)
    }

    /// Return all peers known across every room (deduplicated by PeerId).
    pub fn all_peers(&self) -> Vec<(PeerId, AgentCard)> {
        let r = self.roster.lock().unwrap();
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for room_state in r.values() {
            for (peer_id, card) in &room_state.peers {
                if seen.insert(*peer_id) {
                    result.push((*peer_id, card.clone()));
                }
            }
        }
        result
    }

    pub async fn announce(&self) -> Result<(), P2pError> {
        self.cmd_tx
            .send(P2pCommand::Announce)
            .await
            .map_err(|_| P2pError::Shutdown)
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
                m.insert(
                    room.clone(),
                    RoomState {
                        room: room.clone(),
                        peers: HashMap::new(),
                    },
                );
            }
            m
        }));
        Self {
            config,
            event_tx,
            log_tx,
            cmd_tx,
            cmd_rx,
            roster,
        }
    }

    pub fn handle(&self) -> P2pHandle {
        P2pHandle {
            cmd_tx: self.cmd_tx.clone(),
            event_tx: self.event_tx.clone(),
            log_tx: self.log_tx.clone(),
            roster: Arc::clone(&self.roster),
        }
    }

    /// Build the swarm, dial all discovered relays, then run the event loop.
    ///
    /// Blocks until a `Shutdown` command or Ctrl-C is received.
    pub async fn run(self) -> Result<(), P2pError> {
        let key = match &self.config.keypair_path {
            Some(p) => load_or_create_keypair(p)?,
            None => libp2p::identity::Keypair::generate_ed25519(),
        };
        let local_peer_id = PeerId::from(key.public());
        let mut agent_card = self.config.agent_card.clone();
        agent_card.peer_id = local_peer_id.to_string();
        tracing::info!("P2pNode starting peer_id={local_peer_id}");

        let mut swarm = build_node_swarm(&key, local_peer_id)?;
        swarm
            .listen_on(self.config.listen_addr.clone())
            .map_err(|e| P2pError::Transport(e.to_string()))?;

        let (relay_peers, relay_dial_addrs) =
            fetch_and_dial_relays(&self.config.discovery, &mut swarm).await;

        let state = NodeState {
            local_peer_id,
            agent_card,
            rooms: self.config.rooms.clone(),
            discovery: Arc::clone(&self.config.discovery),
            poll_interval: self.config.discovery_poll_interval,
            event_tx: self.event_tx.clone(),
            roster: Arc::clone(&self.roster),
            relay_peers,
            connected_relay_addrs: HashMap::new(),
            published_relays: HashSet::new(),
            relay_dial_addrs,
            dialed: HashSet::new(),
            rejected: HashSet::new(),
            announced_to: HashSet::new(),
            relay_connection_ids: HashMap::new(),
            pending_inbound: HashMap::new(),
            pending_outbound: HashMap::new(),
        };

        state.event_loop(swarm, self.cmd_rx).await
    }
}

// ── NodeState ─────────────────────────────────────────────────────────────────

/// All mutable state owned by the running event loop.
///
/// Methods are grouped by the event or concern they handle.  The swarm is kept
/// as a separate local variable in `event_loop` so that `tokio::select!` can
/// poll `swarm.select_next_some()` without conflicting with the `&mut self`
/// borrows taken by the handler methods.
struct NodeState {
    local_peer_id: PeerId,
    agent_card: AgentCard,
    rooms: Vec<String>,
    discovery: Arc<dyn DiscoveryProvider>,
    poll_interval: Duration,
    event_tx: broadcast::Sender<P2pEvent>,
    roster: Arc<Mutex<HashMap<String, RoomState>>>,
    /// All relay peer IDs known from discovery (peer ID is embedded in the
    /// git-stored Multiaddr and verified by libp2p's Noise handshake).
    relay_peers: HashSet<PeerId>,
    /// relay peer_id → the address we actually connected on (used to build circuits).
    connected_relay_addrs: HashMap<PeerId, Multiaddr>,
    /// Relay peer IDs for which we have successfully published our circuit address.
    published_relays: HashSet<PeerId>,
    /// Peers we have already dialled (prevents redundant dials).
    dialed: HashSet<PeerId>,
    /// Transport dial addresses (no /p2p suffix) per relay peer ID.
    /// Used to reconnect relays that drop and to detect newly published relays.
    relay_dial_addrs: HashMap<PeerId, Vec<Multiaddr>>,
    /// Peers rejected due to a cryptographic peer-ID mismatch (WrongPeerId).
    /// They have a stale git ref; we skip them until the ref is updated.
    rejected: HashSet<PeerId>,
    /// Peers we have already sent our `AgentCard` to (prevents duplicate announces
    /// when DCUtR upgrades a relayed connection to a direct one).
    announced_to: HashSet<PeerId>,
    /// Active relayed `ConnectionId` per application peer.  When DCUtR establishes
    /// a direct connection we close the relay leg and remove the entry.
    relay_connection_ids: HashMap<PeerId, ConnectionId>,
    /// Inbound task requests awaiting execution: task_id → response channel.
    /// Populated in `on_task_request`; drained by `P2pCommand::TaskReply`.
    pending_inbound: HashMap<Uuid, request_response::ResponseChannel<P2pResponse>>,
    /// Outbound task requests awaiting a `TaskResult` from the remote peer:
    /// OutboundRequestId → oneshot sender.
    /// Populated in `on_command(SendTask)`; fired when the response arrives.
    pending_outbound: HashMap<request_response::OutboundRequestId, TaskReplySender>,
}

impl NodeState {
    // ── Event loop ───────────────────────────────────────────────────────────

    async fn event_loop(
        mut self,
        mut swarm: NodeSwarm,
        mut cmd_rx: mpsc::Receiver<P2pCommand>,
    ) -> Result<(), P2pError> {
        let mut poll = interval_at(Instant::now() + self.poll_interval, self.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let relay_interval = Duration::from_secs(RELAY_POLL_SECS);
        let mut relay_poll = interval_at(Instant::now() + relay_interval, relay_interval);
        relay_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                event = swarm.select_next_some() => {
                    self.on_swarm_event(&mut swarm, event).await;
                }
                _ = poll.tick() => {
                    self.on_poll_tick(&mut swarm).await;
                }
                _ = relay_poll.tick() => {
                    self.on_relay_poll_tick(&mut swarm).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    if self.on_command(&mut swarm, cmd) { break; }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }

        self.cleanup().await;
        Ok(())
    }

    // ── Swarm event dispatch ─────────────────────────────────────────────────

    async fn on_swarm_event(
        &mut self,
        swarm: &mut NodeSwarm,
        event: SwarmEvent<P2pBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!("Listening on {address}");
            }

            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                self.on_connection_established(swarm, peer_id, connection_id, &endpoint);
            }

            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                num_established,
                ..
            } => {
                self.on_connection_closed(swarm, peer_id, connection_id, num_established);
            }

            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.on_external_addr_confirmed(swarm, address).await;
            }

            SwarmEvent::Behaviour(P2pBehaviourEvent::Relay(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                self.on_relay_reservation_accepted(relay_peer_id);
            }

            SwarmEvent::Behaviour(P2pBehaviourEvent::Task(request_response::Event::Message {
                peer,
                message,
                ..
            })) => {
                self.on_task_message(swarm, peer, message);
            }

            SwarmEvent::Behaviour(P2pBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                on_identify_received(swarm, peer_id, info.listen_addrs);
            }

            SwarmEvent::Behaviour(P2pBehaviourEvent::Dcutr(event)) => {
                on_dcutr_event(event);
            }

            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                self.on_outgoing_error(peer_id, error);
            }

            _ => {}
        }
    }

    // ── Connection lifecycle ─────────────────────────────────────────────────

    fn on_connection_established(
        &mut self,
        swarm: &mut NodeSwarm,
        peer_id: PeerId,
        connection_id: ConnectionId,
        endpoint: &ConnectedPoint,
    ) {
        let via_relay = endpoint.is_relayed();
        self.emit(P2pEvent::Connected { peer_id, via_relay });

        if self.relay_peers.contains(&peer_id) {
            // Register a circuit-relay listen address for every relay we connect
            // to so peers can reach us through any available relay server.
            if let std::collections::hash_map::Entry::Vacant(e) =
                self.connected_relay_addrs.entry(peer_id)
            {
                let relay_addr = endpoint.get_remote_address().clone();
                tracing::info!("Relay {peer_id} reachable at {relay_addr}");
                e.insert(relay_addr.clone());
                let circuit = mk_circuit_addr(&relay_addr, self.local_peer_id);
                if let Err(e) = swarm.listen_on(circuit) {
                    tracing::warn!("relay listen_on failed: {e}");
                }
            }
        } else if via_relay {
            // Application peer reachable only via relay — record the connection
            // so we can close it once DCUtR successfully upgrades to direct.
            self.relay_connection_ids.insert(peer_id, connection_id);
            if self.announced_to.insert(peer_id) {
                swarm
                    .behaviour_mut()
                    .task
                    .send_request(&peer_id, P2pRequest::Announce(self.agent_card.clone()));
            }
        } else {
            // Direct (non-relayed) connection — if DCUtR just upgraded a relay
            // connection, close the relay leg now that we have a better path.
            if let Some(relay_conn) = self.relay_connection_ids.remove(&peer_id) {
                tracing::info!(
                    "Direct connection to {peer_id} established;                      closing relayed fallback connection"
                );
                swarm.close_connection(relay_conn);
            }
            if self.announced_to.insert(peer_id) {
                swarm
                    .behaviour_mut()
                    .task
                    .send_request(&peer_id, P2pRequest::Announce(self.agent_card.clone()));
            }
        }
    }

    fn on_connection_closed(
        &mut self,
        swarm: &mut NodeSwarm,
        peer_id: PeerId,
        connection_id: ConnectionId,
        num_established: u32,
    ) {
        // Keep relay_connection_ids consistent when a relayed connection
        // closes externally (circuit expired, relay restarted, etc.).
        if let Some(stored) = self.relay_connection_ids.get(&peer_id) {
            if *stored == connection_id {
                self.relay_connection_ids.remove(&peer_id);
            }
        }

        if num_established > 0 {
            return;
        }

        if self.relay_peers.contains(&peer_id) {
            // A relay server lost all connections — clean up dependent state and
            // immediately attempt a reconnect.  The relay_poll will keep retrying
            // at RELAY_POLL_SECS cadence if the first attempt fails.
            tracing::info!("Relay {peer_id} disconnected; attempting reconnect");
            self.connected_relay_addrs.remove(&peer_id);
            self.published_relays.remove(&peer_id);
            if let Some(addrs) = self.relay_dial_addrs.get(&peer_id).cloned() {
                if let Err(e) = swarm.dial(DialOpts::peer_id(peer_id).addresses(addrs).build()) {
                    tracing::debug!("Immediate relay redial {peer_id}: {e}");
                }
            }
        } else {
            // Application peer fully disconnected — allow the peer poll to
            // rediscover and re-dial them if they reappear in the registry.
            self.dialed.remove(&peer_id);
            self.announced_to.remove(&peer_id);
            self.emit(P2pEvent::Disconnected { peer_id });
        }
    }

    // ── Relay / circuit ──────────────────────────────────────────────────────

    /// Called when libp2p confirms an external address.  For circuit addresses
    /// this is the signal that the relay reservation is live and we can publish
    /// our reachable address for the first time on this relay.
    async fn on_external_addr_confirmed(&mut self, swarm: &mut NodeSwarm, address: Multiaddr) {
        if !address.to_string().contains("p2p-circuit") {
            return;
        }
        let relay_pid = relay_peer_from_circuit_addr(&address);
        // Only publish once per relay to avoid redundant git pushes.
        let is_new = relay_pid.is_none_or(|rp| self.published_relays.insert(rp));
        if !is_new {
            return;
        }

        tracing::info!("Relay circuit confirmed: {address}");
        self.emit(P2pEvent::RelayCircuitEstablished {
            relay_peer_id: relay_pid,
        });
        self.publish_peer_via_circuit(address);
        self.fetch_and_dial_peers(swarm).await;
    }

    /// Fallback: if `ExternalAddrConfirmed` never fires (e.g. strict NAT without
    /// AutoNAT), the `ReservationReqAccepted` event is used to publish instead.
    fn on_relay_reservation_accepted(&mut self, relay_peer_id: PeerId) {
        tracing::info!("Relay reservation accepted by {relay_peer_id}");
        if self.published_relays.contains(&relay_peer_id) {
            return;
        }
        let Some(relay_addr) = self.connected_relay_addrs.get(&relay_peer_id).cloned() else {
            return;
        };
        self.published_relays.insert(relay_peer_id);
        self.emit(P2pEvent::RelayCircuitEstablished {
            relay_peer_id: Some(relay_peer_id),
        });
        let circuit = mk_circuit_addr(&relay_addr, self.local_peer_id);
        let disc = Arc::clone(&self.discovery);
        let rooms = self.rooms.clone();
        let local = self.local_peer_id;
        tokio::task::spawn_blocking(move || {
            for room in &rooms {
                if let Err(e) = disc.publish_peer(room, &local, &circuit) {
                    tracing::warn!("fallback publish_peer failed: {e}");
                }
            }
        });
    }

    // ── Request/response messages ────────────────────────────────────────────

    fn on_task_message(
        &mut self,
        swarm: &mut NodeSwarm,
        peer: PeerId,
        message: request_response::Message<P2pRequest, P2pResponse>,
    ) {
        match message {
            request_response::Message::Request {
                request, channel, ..
            } => match request {
                P2pRequest::Announce(card) => self.on_announce_request(swarm, peer, card, channel),
                P2pRequest::Task(req) => self.on_task_request(swarm, peer, req, channel),
            },
            request_response::Message::Response {
                request_id,
                response,
                ..
            } => {
                if let P2pResponse::TaskResult(resp) = response {
                    // Fire the waiting oneshot for the caller of send_task().
                    if let Some(reply_tx) = self.pending_outbound.remove(&request_id) {
                        let _ = reply_tx.send(Ok(resp.clone()));
                    }
                    self.on_task_response(peer, resp);
                }
            }
        }
    }

    fn on_announce_request(
        &mut self,
        swarm: &mut NodeSwarm,
        peer: PeerId,
        card: AgentCard,
        channel: request_response::ResponseChannel<P2pResponse>,
    ) {
        {
            let mut r = self.roster.lock().unwrap();
            for room in &self.rooms {
                r.entry(room.clone())
                    .or_insert_with(|| RoomState {
                        room: room.clone(),
                        peers: HashMap::new(),
                    })
                    .peers
                    .insert(peer, card.clone());
            }
        }
        for room in &self.rooms {
            self.emit(P2pEvent::PeerDiscovered {
                room: room.clone(),
                peer_id: peer,
                card: card.clone(),
            });
        }
        let _ = swarm
            .behaviour_mut()
            .task
            .send_response(channel, P2pResponse::Ack);
    }

    fn on_task_request(
        &mut self,
        _swarm: &mut NodeSwarm,
        peer: PeerId,
        req: TaskRequest,
        channel: request_response::ResponseChannel<P2pResponse>,
    ) {
        let id = req.id;
        // Store the response channel keyed by task ID.  The task executor
        // (running in the gateway) will call P2pHandle::reply_to_task(id, ...)
        // which sends P2pCommand::TaskReply back to this event loop, where we
        // look up the channel and call send_response.  This keeps the
        // ResponseChannel inside the swarm event loop (the only place allowed
        // to call send_response) and keeps P2pEvent clean.
        self.pending_inbound.insert(id, channel);
        self.emit(P2pEvent::TaskRequested {
            id,
            from: peer,
            request: req,
        });
    }

    fn on_task_response(&self, peer: PeerId, resp: TaskResponse) {
        self.emit(P2pEvent::TaskResponseReceived {
            id: resp.request_id,
            from: peer,
            response: resp,
        });
    }

    // ── Dial error ────────────────────────────────────────────────────────────────────────────

    fn on_outgoing_error(&mut self, peer_id: Option<PeerId>, error: DialError) {
        if let DialError::WrongPeerId { obtained, .. } = &error {
            if peer_id
                .as_ref()
                .is_some_and(|p| self.relay_peers.contains(p))
            {
                // Relay restarted with a different keypair — stale git ref.
                // The relay poll will redial using freshly-fetched addresses.
                tracing::warn!(
                    "Stale relay ref: expected {:?} but relay presented {obtained};                      waiting for updated git ref.",
                    peer_id
                );
            } else {
                // Cryptographic mismatch for an app peer — the git ref points to
                // a peer that has rotated its key.  Skip until the ref is updated.
                tracing::warn!("WrongPeerId from app peer {peer_id:?}: {error}");
                if let Some(pid) = peer_id {
                    self.rejected.insert(pid);
                    self.dialed.remove(&pid);
                }
            }
        } else if peer_id
            .as_ref()
            .is_some_and(|p| self.relay_peers.contains(p))
        {
            // Relay dial failure (relay may be temporarily down).
            // The relay_poll background timer will retry automatically.
            tracing::debug!("Relay {peer_id:?} unreachable: {error}");
        } else {
            // Transient error to an app peer — remove from dialed so the next
            // peer poll can attempt a fresh dial if the peer is still in git.
            tracing::debug!("Connection error to {peer_id:?}: {error}");
            if let Some(pid) = peer_id {
                self.dialed.remove(&pid);
            }
        }
    }
    // ── Periodic discovery polls ─────────────────────────────────────────────

    async fn on_poll_tick(&mut self, swarm: &mut NodeSwarm) {
        if !self.published_relays.is_empty() {
            self.fetch_and_dial_peers(swarm).await;
        }
    }

    /// Runs every `RELAY_POLL_SECS` seconds.
    ///
    /// 1. Re-fetches relay addresses from the discovery backend so that newly
    ///    published relays are picked up automatically.
    /// 2. Re-dials every known relay that is not currently connected so that
    ///    transient relay outages are recovered from without user intervention.
    async fn on_relay_poll_tick(&mut self, swarm: &mut NodeSwarm) {
        let disc = Arc::clone(&self.discovery);
        let relay_addrs = match tokio::task::spawn_blocking(move || disc.fetch_relay_addrs()).await
        {
            Ok(Ok(addrs)) => addrs,
            Ok(Err(e)) => {
                tracing::debug!("relay poll fetch error: {e}");
                return;
            }
            Err(e) => {
                tracing::debug!("relay poll spawn error: {e}");
                return;
            }
        };

        // Build per-relay transport addresses (strip /p2p suffix).
        let mut fresh: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
        for addr in &relay_addrs {
            if let Some(pid) = peer_id_from_addr(addr) {
                let mut t = addr.clone();
                if matches!(t.iter().last(), Some(Protocol::P2p(_))) {
                    t.pop();
                }
                fresh.entry(pid).or_default().push(t);
            }
        }

        // Incorporate newly discovered relays.
        for (pid, addrs) in &fresh {
            if self.relay_peers.insert(*pid) {
                tracing::info!("Discovered new relay {pid}");
            }
            self.relay_dial_addrs.insert(*pid, addrs.clone());
        }

        // Re-dial every relay we know about but are not currently connected to.
        for (pid, addrs) in &self.relay_dial_addrs.clone() {
            if !self.connected_relay_addrs.contains_key(pid) {
                tracing::info!("Re-dialing relay {pid}");
                if let Err(e) = swarm.dial(DialOpts::peer_id(*pid).addresses(addrs.clone()).build())
                {
                    tracing::debug!("Relay redial {pid}: {e}");
                }
            }
        }
    }

    // ── Command handling ─────────────────────────────────────────────────────

    /// Returns `true` when the loop should exit.
    fn on_command(&mut self, swarm: &mut NodeSwarm, cmd: P2pCommand) -> bool {
        match cmd {
            P2pCommand::SendTask {
                peer,
                request,
                reply_tx,
            } => {
                let req_id = swarm
                    .behaviour_mut()
                    .task
                    .send_request(&peer, P2pRequest::Task(request));
                self.pending_outbound.insert(req_id, reply_tx);
                false
            }
            P2pCommand::TaskReply { id, response } => {
                if let Some(channel) = self.pending_inbound.remove(&id) {
                    if swarm
                        .behaviour_mut()
                        .task
                        .send_response(channel, response)
                        .is_err()
                    {
                        tracing::warn!("TaskReply {id}: failed to send response (channel expired)");
                    }
                } else {
                    tracing::warn!("TaskReply {id}: no pending inbound channel (already replied?)");
                }
                false
            }
            P2pCommand::Announce => {
                let connected: Vec<PeerId> = swarm.connected_peers().copied().collect();
                for peer in connected {
                    if !self.relay_peers.contains(&peer) {
                        swarm
                            .behaviour_mut()
                            .task
                            .send_request(&peer, P2pRequest::Announce(self.agent_card.clone()));
                    }
                }
                false
            }
            P2pCommand::Shutdown => true,
        }
    }

    // ── Discovery helpers ────────────────────────────────────────────────────

    /// Fetch all peers from every room and dial any that are new and reachable.
    async fn fetch_and_dial_peers(&mut self, swarm: &mut NodeSwarm) {
        let disc = Arc::clone(&self.discovery);
        let rooms = self.rooms.clone();
        let all: Vec<Vec<PeerInfo>> = match tokio::task::spawn_blocking(move || {
            rooms
                .iter()
                .map(|r| disc.fetch_peers(r))
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        {
            Ok(Ok(v)) => v,
            _ => return,
        };

        for info in all.into_iter().flatten() {
            self.try_dial_peer(swarm, info);
        }
    }

    fn try_dial_peer(&mut self, swarm: &mut NodeSwarm, info: PeerInfo) {
        if info.peer_id == self.local_peer_id {
            return;
        }
        if self.rejected.contains(&info.peer_id) {
            return;
        }
        // Only dial peers whose published relay is one we are connected to.
        if peer_id_from_addr(&info.relay_addr).is_none_or(|r| !self.relay_peers.contains(&r)) {
            return;
        }
        if self.dialed.insert(info.peer_id) {
            tracing::info!("Dialing {} via relay", info.peer_id);
            if let Err(e) = swarm.dial(info.relay_addr) {
                tracing::warn!("dial failed: {e}");
            }
        }
    }

    /// Publish our circuit address to all rooms in git.  Fire-and-forget via
    /// `spawn_blocking` so the event loop is not blocked.
    fn publish_peer_via_circuit(&self, circuit_addr: Multiaddr) {
        let disc = Arc::clone(&self.discovery);
        let rooms = self.rooms.clone();
        let local = self.local_peer_id;
        tokio::task::spawn_blocking(move || {
            for room in &rooms {
                if let Err(e) = disc.publish_peer(room, &local, &circuit_addr) {
                    tracing::warn!("publish_peer failed: {e}");
                } else {
                    tracing::info!("Published refs/peers/{room}/{local}");
                }
            }
        });
    }

    // ── Shutdown ─────────────────────────────────────────────────────────────

    async fn cleanup(self) {
        if self.published_relays.is_empty() {
            return;
        }
        let disc = Arc::clone(&self.discovery);
        let rooms = self.rooms.clone();
        let local = self.local_peer_id;
        tokio::task::spawn_blocking(move || {
            for room in &rooms {
                if let Err(e) = disc.delete_peer(room, &local) {
                    tracing::warn!("cleanup delete_peer: {e}");
                }
            }
        })
        .await
        .ok();
        tracing::info!("P2pNode shut down");
    }

    // ── Emit helper ──────────────────────────────────────────────────────────

    fn emit(&self, event: P2pEvent) {
        let _ = self.event_tx.send(event);
    }
}

// ── Setup helpers ─────────────────────────────────────────────────────────────

/// Build a combined TCP + relay-client transport and return a ready swarm.
fn build_node_swarm(
    key: &libp2p::identity::Keypair,
    local_peer_id: PeerId,
) -> Result<NodeSwarm, P2pError> {
    let (relay_transport, relay_client) = relay::client::new(local_peer_id);

    let tcp_t = tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::Config::new(key).map_err(|e| P2pError::Transport(e.to_string()))?)
        .multiplex(yamux::Config::default())
        .map(|(p, m), _| (p, StreamMuxerBox::new(m)));

    let relay_t = relay_transport
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::Config::new(key).map_err(|e| P2pError::Transport(e.to_string()))?)
        .multiplex(yamux::Config::default())
        .map(|(p, m), _| (p, StreamMuxerBox::new(m)));

    let transport = tcp_t
        .or_transport(relay_t)
        .map(|either, _| match either {
            future::Either::Left(v) => v,
            future::Either::Right(v) => v,
        })
        .boxed();

    let behaviour = P2pBehaviour::new(key, relay_client);
    Ok(Swarm::new(
        transport,
        behaviour,
        local_peer_id,
        default_swarm_config(),
    ))
}

/// Fetch all relay addresses from discovery, group them by peer ID, and dial
/// each relay server.
///
/// Returns empty sets when no relays are configured — the node will still
/// work via mDNS for local peers.  Relay support is additive.
async fn fetch_and_dial_relays(
    discovery: &Arc<dyn DiscoveryProvider>,
    swarm: &mut NodeSwarm,
) -> (HashSet<PeerId>, HashMap<PeerId, Vec<Multiaddr>>) {
    let disc = Arc::clone(discovery);
    let relay_addrs = match tokio::task::spawn_blocking(move || disc.fetch_relay_addrs()).await {
        Ok(Ok(addrs)) => addrs,
        Ok(Err(e)) => {
            tracing::debug!("No relay addresses found (discovery: {e}); running without relay");
            return (HashSet::new(), HashMap::new());
        }
        Err(e) => {
            tracing::debug!("Relay discovery task error: {e}; running without relay");
            return (HashSet::new(), HashMap::new());
        }
    };

    let relay_peers: HashSet<PeerId> = relay_addrs.iter().filter_map(peer_id_from_addr).collect();
    if relay_peers.is_empty() {
        tracing::debug!(
            "Discovery returned addresses but none had /p2p component; running without relay"
        );
        return (HashSet::new(), HashMap::new());
    }

    // Group transport addresses (strip /p2p suffix) by relay peer ID so we can
    // pass all addresses for a given relay to a single DialOpts call and so that
    // NodeState can use them for reconnection later.
    let mut relay_dial_addrs: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    for addr in &relay_addrs {
        if let Some(pid) = peer_id_from_addr(addr) {
            let mut t = addr.clone();
            if matches!(t.iter().last(), Some(Protocol::P2p(_))) {
                t.pop();
            }
            relay_dial_addrs.entry(pid).or_default().push(t);
        }
    }

    for (pid, addrs) in &relay_dial_addrs {
        tracing::info!("Dialing relay {pid} with {} address(es)", addrs.len());
        if let Err(e) = swarm.dial(DialOpts::peer_id(*pid).addresses(addrs.clone()).build()) {
            tracing::warn!("Failed to dial relay {pid}: {e}");
        }
    }

    (relay_peers, relay_dial_addrs)
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

fn peer_id_from_addr(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(mh) => PeerId::from_multihash(mh.into()).ok(),
        _ => None,
    })
}

/// Extract the relay server's `PeerId` from a circuit address.
///
/// Circuit address shape:
/// `<transport>/p2p/<relay-peer-id>/p2p-circuit/p2p/<our-peer-id>`
///
/// Returns the `/p2p` peer-id seen immediately before the `/p2p-circuit`
/// component, which identifies the relay server.
fn relay_peer_from_circuit_addr(addr: &Multiaddr) -> Option<PeerId> {
    let mut last_peer: Option<PeerId> = None;
    for proto in addr.iter() {
        match proto {
            Protocol::P2pCircuit => return last_peer,
            Protocol::P2p(mh) => last_peer = PeerId::from_multihash(mh.into()).ok(),
            _ => {}
        }
    }
    None
}

fn mk_circuit_addr(relay_addr: &Multiaddr, target: PeerId) -> Multiaddr {
    let mut a = relay_addr.clone();
    a.push(Protocol::P2pCircuit);
    a.push(Protocol::P2p(target));
    a
}

/// Feed identify addresses directly into the swarm's address book.
fn on_identify_received(swarm: &mut NodeSwarm, peer_id: PeerId, addrs: Vec<Multiaddr>) {
    for addr in addrs {
        swarm.add_peer_address(peer_id, addr);
    }
}

/// Log DCUtR hole-punching outcome.
///
/// DCUtR fires one event per upgrade attempt: `result` is `Ok(ConnectionId)`
/// on success (the new direct connection) or `Err` if all attempts failed.
/// The relay connection is closed separately in `on_connection_established`
/// once the direct `ConnectionEstablished` event fires.
fn on_dcutr_event(event: dcutr::Event) {
    let dcutr::Event {
        remote_peer_id,
        result,
    } = event;
    match result {
        Ok(_conn) => {
            tracing::info!("DCUtR: direct connection to {remote_peer_id} established");
        }
        Err(e) => {
            tracing::debug!(
                "DCUtR: hole-punch to {remote_peer_id} failed ({e});                  relay connection remains"
            );
        }
    }
}
