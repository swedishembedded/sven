//! `sven-p2p-client` — standalone P2P chat/task client.
//!
//! Connects to a relay server (address read from the discovery backend),
//! joins one or more rooms, and provides:
//!
//! **TUI mode** (default)
//!   ┌─ sven-p2p-client · room: devs ──────────────── peers: alice bob ─┐
//!   │ [alice] hello everyone                                             │
//!   │ [bob]   hey alice!                                                 │
//!   │ * charlie joined                                                   │
//!   └─────────────────────────────────────────────────────────────────── ┘
//!   > @alice how are you?_
//!
//!   Type `@name <message>` to send to a specific peer.
//!   Type `<message>` (no @) to broadcast to all peers in the room.
//!   Press Esc or Ctrl-C to quit.
//!
//! **One-shot mode** (with -m / --message)
//!   Connects, sends the message, then exits.
//!   `@name` prefix in the message routes to that specific peer.
//!
//! # Examples
//!
//! ```sh
//! # Start TUI
//! sven-p2p-client --repo . --room devs --name alice
//!
//! # One-shot send to bob
//! sven-p2p-client --repo . --room devs --name alice -m "@bob hello!"
//!
//! # One-shot broadcast
//! sven-p2p-client --repo . --room devs --name alice -m "hello everyone"
//! ```

use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::broadcast;

use sven_p2p::{
    config::P2pConfig,
    discovery::{memory::InMemoryDiscovery, DiscoveryProvider},
    node::{P2pEvent, P2pHandle, P2pNode},
    protocol::types::AgentCard,
};

#[cfg(feature = "git-discovery")]
use sven_p2p::discovery::git::GitDiscoveryProvider;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "sven-p2p-client",
    about = "Lightweight P2P chat/task client for sven agent rooms",
    long_about = None
)]
struct Args {
    /// Path to the git repository used for peer discovery.
    /// Omit to use an in-memory (local-only) discovery backend.
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Room to join (can be specified multiple times).
    #[arg(long = "room", short = 'r', required_unless_present = "list_relays")]
    rooms: Vec<String>,

    /// Your display name. Must not contain spaces.
    #[arg(long, short = 'n', required_unless_present = "list_relays")]
    name: Option<String>,

    /// TCP listen address.
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
    listen: libp2p::Multiaddr,

    /// Path to persist your keypair (so your PeerId stays stable across runs).
    #[arg(long)]
    keypair: Option<PathBuf>,

    /// One-shot mode: send this message and exit.
    /// Use `@name message` to target a specific peer; omit `@name` to broadcast.
    #[arg(long = "message", short = 'm')]
    message: Option<String>,

    /// Debug: list the relay servers found in the discovery backend and exit.
    /// Shows each relay's peer ID and all its listen addresses.
    #[arg(long, conflicts_with_all = ["message"])]
    list_relays: bool,
}

// ── App state (shared between TUI and event handler) ─────────────────────────

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<AppInner>>,
}

struct AppInner {
    messages: Vec<ChatLine>,
    /// name → PeerId string
    peers: HashMap<String, String>,
    input: String,
    /// Scroll offset for the message pane (0 = bottom).
    scroll_offset: u16,
    quit: bool,
}

#[derive(Clone)]
enum ChatLine {
    Message { from: String, body: String },
    System(String),
}

impl ChatLine {
    fn msg(from: impl Into<String>, body: impl Into<String>) -> Self {
        ChatLine::Message {
            from: from.into(),
            body: body.into(),
        }
    }
    fn sys(text: impl Into<String>) -> Self {
        ChatLine::System(text.into())
    }
}

impl AppState {
    fn new() -> Self {
        AppState {
            inner: Arc::new(Mutex::new(AppInner {
                messages: Vec::new(),
                peers: HashMap::new(),
                input: String::new(),
                scroll_offset: 0,
                quit: false,
            })),
        }
    }

    fn push(&self, line: ChatLine) {
        let mut g = self.inner.lock().unwrap();
        g.messages.push(line);
        g.scroll_offset = 0; // snap to bottom
    }

    fn add_peer(&self, name: String, peer_id: String) {
        self.inner.lock().unwrap().peers.insert(name, peer_id);
    }

    fn remove_peer(&self, peer_id: &str) {
        let mut g = self.inner.lock().unwrap();
        g.peers.retain(|_, v| v != peer_id);
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // --list-relays is a debug command: open discovery, print relays, exit.
    if args.list_relays {
        let discovery = open_discovery(&args.repo)?;
        return list_relays_cmd(&*discovery);
    }

    // name and rooms are required for all other modes (clap enforces this).
    let name = args.name.as_deref().expect("--name is required");

    // Basic validation.
    if name.contains(' ') {
        anyhow::bail!("--name must not contain spaces");
    }

    // Set up tracing (non-TUI mode uses RUST_LOG; TUI mode silences it).
    if args.message.is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn".parse().unwrap()),
            )
            .init();
    }

    let discovery = open_discovery(&args.repo)?;

    let card = AgentCard {
        peer_id: String::new(), // filled in by P2pNode::run
        name: name.to_string(),
        description: format!("{} (sven-p2p-client)", name),
        capabilities: vec!["chat".into()],
        version: env!("CARGO_PKG_VERSION").into(),
    };

    let config = P2pConfig {
        listen_addr: args.listen.clone(),
        rooms: args.rooms.clone(),
        agent_card: card,
        discovery,
        keypair_path: args.keypair.clone(),
        discovery_poll_interval: Duration::from_secs(5),
    };

    let node = P2pNode::new(config);
    let handle = node.handle();

    tokio::spawn(async move {
        if let Err(e) = node.run().await {
            eprintln!("P2P node error: {e}");
        }
    });

    if let Some(msg) = args.message {
        run_oneshot(handle, name, &args.rooms[0], msg).await
    } else {
        run_tui(handle, name, &args.rooms).await
    }
}

// ── Setup helpers ────────────────────────────────────────────────────────────

/// Open the discovery backend selected by `--repo`.
fn open_discovery(repo: &Option<PathBuf>) -> anyhow::Result<Arc<dyn DiscoveryProvider>> {
    match repo {
        #[cfg(feature = "git-discovery")]
        Some(path) => Ok(Arc::new(
            GitDiscoveryProvider::open(path).map_err(|e| anyhow::anyhow!("git repo error: {e}"))?,
        )),
        _ => {
            if repo.is_some() {
                eprintln!("warning: --repo given but git-discovery feature is disabled; using in-memory backend");
            }
            Ok(Arc::new(InMemoryDiscovery::new()))
        }
    }
}

/// `--list-relays`: fetch relay addresses from the discovery backend and print
/// them grouped by relay peer ID, then return.
///
/// Each `refs/relay/<sha256>` blob in git contains one Multiaddr of the form:
///   `/ip4/<ip>/tcp/<port>/p2p/<peer-id>`
///
/// The `/p2p/<peer-id>` component is the relay's libp2p identity; libp2p's
/// Noise handshake verifies it on every connection.
fn list_relays_cmd(discovery: &dyn DiscoveryProvider) -> anyhow::Result<()> {
    match discovery.fetch_relay_addrs() {
        Ok(addrs) => {
            if addrs.is_empty() {
                println!("No relay addresses found.");
                return Ok(());
            }

            // Group by peer ID (extracted from the /p2p/<peer-id> component).
            let mut by_peer: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for addr in &addrs {
                let peer_id = relay_peer_id_str(addr);
                by_peer.entry(peer_id).or_default().push(addr.to_string());
            }

            println!("Relay server(s) found: {}", by_peer.len());
            for (i, (peer_id, peer_addrs)) in by_peer.iter().enumerate() {
                println!("  [{}] PeerId: {}", i + 1, peer_id);
                for addr in peer_addrs {
                    println!("       {addr}");
                }
            }
        }
        Err(e) => {
            println!("No relays found in discovery backend: {e}");
        }
    }
    Ok(())
}

/// Extract the peer ID string from the `/p2p/<peer-id>` component of a Multiaddr.
fn relay_peer_id_str(addr: &libp2p::Multiaddr) -> String {
    use libp2p::multiaddr::Protocol;
    addr.iter()
        .find_map(|p| match p {
            Protocol::P2p(mh) => libp2p::PeerId::from_multihash(mh.into())
                .ok()
                .map(|pid| pid.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "(no peer id)".to_string())
}

// ── One-shot mode ─────────────────────────────────────────────────────────────

async fn run_oneshot(
    handle: P2pHandle,
    own_name: &str,
    room: &str,
    message: String,
) -> anyhow::Result<()> {
    let mut events = handle.subscribe_events();
    // Wait until connected and circuit is confirmed (PeerDiscovered or Connected event),
    // or time out after 30s.
    let timeout = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(timeout);

    let (target_name, body) = parse_message(&message);

    println!("Connecting to room '{room}'…");

    // Wait for the relay circuit to be confirmed (RelayCircuitEstablished) and
    // then for a peer to be discovered, or time out after 30 s and send with
    // whoever is currently in the room.
    //
    // We deliberately do NOT break on the raw `Connected` event: that fires the
    // instant the TCP connection to the relay opens — long before the circuit
    // reservation is accepted, our address is published to git, or any peer
    // dials us back.
    loop {
        tokio::select! {
            Ok(ev) = events.recv() => {
                match ev {
                    P2pEvent::RelayCircuitEstablished { relay_peer_id } => {
                        let pid = relay_peer_id
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "?".into());
                        println!("Relay circuit ready (via {pid}), looking for peers…");
                        // Stay in the loop: peers that are already in the room
                        // will be dialled immediately after we publish our address.
                    }
                    P2pEvent::PeerDiscovered { .. } => {
                        break; // a peer dialled us — send immediately
                    }
                    _ => {}
                }
            }
            _ = &mut timeout => {
                // 30 s elapsed; proceed to send with whoever is in the room now.
                break;
            }
        }
    }

    // Send.
    let peers = handle.room_peers(room);
    let sent = send_message(&handle, own_name, &peers, target_name.as_deref(), &body).await;
    if sent == 0 {
        if target_name.is_some() {
            anyhow::bail!(
                "peer '@{}' not found in room '{room}'",
                target_name.unwrap()
            );
        } else {
            println!("(no peers in room '{room}' to broadcast to)");
        }
    } else {
        println!("Sent to {sent} peer(s).");
    }

    // Small grace period so the message is actually transmitted.
    tokio::time::sleep(Duration::from_millis(400)).await;
    Ok(())
}

// ── TUI mode ──────────────────────────────────────────────────────────────────

async fn run_tui(handle: P2pHandle, own_name: &str, rooms: &[String]) -> anyhow::Result<()> {
    let primary_room = rooms[0].clone();
    let own_name = own_name.to_string();

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let state = AppState::new();

    // Spawn event-listener task.
    let state_bg = state.clone();
    let handle_bg = handle.clone();
    let room_bg = primary_room.clone();
    tokio::spawn(async move {
        listen_events(handle_bg, state_bg, room_bg).await;
    });

    // Main TUI loop.
    let tick = Duration::from_millis(50);
    let result = loop {
        // Draw.
        let state_snap = {
            let g = state.inner.lock().unwrap();
            (
                g.messages.clone(),
                g.peers.clone(),
                g.input.clone(),
                g.scroll_offset,
                g.quit,
            )
        };
        if state_snap.4 {
            break Ok(());
        }

        terminal.draw(|f| {
            draw_ui(
                f,
                &state_snap.0,
                &state_snap.1,
                &state_snap.2,
                &own_name,
                &primary_room,
            );
        })?;

        // Input events (with timeout so we keep redrawing).
        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                    | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        handle.shutdown().await;
                        break Ok(());
                    }
                    (KeyCode::Enter, _) => {
                        let input = {
                            let mut g = state.inner.lock().unwrap();
                            let s = g.input.trim().to_string();
                            g.input.clear();
                            s
                        };
                        if !input.is_empty() {
                            let (target, body) = parse_message(&input);
                            let peers = handle.room_peers(&primary_room);
                            let sent =
                                send_message(&handle, &own_name, &peers, target.as_deref(), &body)
                                    .await;
                            if sent > 0 {
                                let display = match &target {
                                    Some(t) => format!("[@{t}] {body}"),
                                    None => format!("[broadcast] {body}"),
                                };
                                state.push(ChatLine::msg(format!("{own_name} (you)"), display));
                            } else if let Some(t) = &target {
                                state.push(ChatLine::sys(format!("! peer @{t} not found")));
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        state.inner.lock().unwrap().input.pop();
                    }
                    (KeyCode::Up, _) => {
                        let mut g = state.inner.lock().unwrap();
                        g.scroll_offset = g.scroll_offset.saturating_add(1);
                    }
                    (KeyCode::Down, _) => {
                        let mut g = state.inner.lock().unwrap();
                        g.scroll_offset = g.scroll_offset.saturating_sub(1);
                    }
                    (KeyCode::PageUp, _) => {
                        let mut g = state.inner.lock().unwrap();
                        g.scroll_offset = g.scroll_offset.saturating_add(10);
                    }
                    (KeyCode::PageDown, _) => {
                        let mut g = state.inner.lock().unwrap();
                        g.scroll_offset = g.scroll_offset.saturating_sub(10);
                    }
                    (KeyCode::Char(c), _) => {
                        state.inner.lock().unwrap().input.push(c);
                    }
                    _ => {}
                }
            }
        }
    };

    // Restore terminal.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

// ── Background event listener ─────────────────────────────────────────────────

async fn listen_events(handle: P2pHandle, state: AppState, room: String) {
    let mut events = handle.subscribe_events();
    loop {
        match events.recv().await {
            Ok(ev) => match ev {
                P2pEvent::PeerDiscovered {
                    peer_id,
                    card,
                    room: r,
                    ..
                } if r == room => {
                    let already_known = {
                        let g = state.inner.lock().unwrap();
                        g.peers.contains_key(&card.name)
                    };
                    state.add_peer(card.name.clone(), peer_id.to_string());
                    if !already_known {
                        state.push(ChatLine::sys(format!("* {} joined", card.name)));
                    }
                }
                P2pEvent::PeerLeft { peer_id, .. } => {
                    state.remove_peer(&peer_id.to_string());
                    state.push(ChatLine::sys(format!("* {} left", peer_id)));
                }
                P2pEvent::RelayCircuitEstablished { relay_peer_id } => {
                    let pid = relay_peer_id
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "?".into());
                    state.push(ChatLine::sys(format!("~ relay circuit ready (via {pid})")));
                }
                P2pEvent::Connected { peer_id, via_relay } => {
                    let how = if via_relay { "via relay" } else { "direct" };
                    state.push(ChatLine::sys(format!("~ connected to {peer_id} ({how})")));
                }
                P2pEvent::Disconnected { peer_id } => {
                    state.push(ChatLine::sys(format!("~ disconnected from {peer_id}")));
                }
                P2pEvent::Error(e) => {
                    state.push(ChatLine::sys(format!("! error: {e}")));
                }
                P2pEvent::TaskRequested { from, request, .. } => {
                    // All messages — plain chat or multimodal tasks — arrive here.
                    // Resolve sender name from the room roster (falls back to peer_id string).
                    let name = {
                        let g = state.inner.lock().unwrap();
                        g.peers
                            .iter()
                            .find(|(_, pid)| *pid == &from.to_string())
                            .map(|(n, _)| n.clone())
                            .unwrap_or_else(|| from.to_string())
                    };
                    // Plain message -> description only; multimodal -> note the extra blocks.
                    let body = if request.payload.is_empty() {
                        request.description.clone()
                    } else {
                        format!(
                            "{} [+{} block(s)]",
                            request.description,
                            request.payload.len()
                        )
                    };
                    state.push(ChatLine::msg(name, body));
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

// ── TUI rendering ─────────────────────────────────────────────────────────────

fn draw_ui(
    f: &mut Frame,
    messages: &[ChatLine],
    peers: &HashMap<String, String>,
    input: &str,
    own_name: &str,
    room: &str,
) {
    let size = f.area();

    // Outer layout: title + body + input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(3),    // message + peers
            Constraint::Length(3), // input box
        ])
        .split(size);

    // ── Title bar ──────────────────────────────────────────────────────────
    let peer_names: Vec<&str> = peers.keys().map(|s| s.as_str()).collect();
    let peer_list = if peer_names.is_empty() {
        "(no peers)".to_string()
    } else {
        peer_names.join("  ")
    };
    let title = Line::from(vec![
        Span::styled(
            format!(" sven-p2p-client · room: {room} · you: {own_name} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("── peers: {peer_list} "),
            Style::default().fg(Color::Green),
        ),
    ]);
    f.render_widget(Paragraph::new(title), chunks[0]);

    // ── Message pane ───────────────────────────────────────────────────────
    let msg_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " messages ",
            Style::default().fg(Color::DarkGray),
        ));

    let inner_height = chunks[1].height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = messages
        .iter()
        .map(|line| match line {
            ChatLine::Message { from, body } => ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{from}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ▶ "),
                Span::raw(body.clone()),
            ])),
            ChatLine::System(text) => ListItem::new(Span::styled(
                format!("  {text}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        })
        .collect();

    // Compute scroll: how many items to skip from the top so that the
    // last `inner_height` items are visible, adjusted by scroll_offset.
    let total = items.len();
    let skip = if total > inner_height {
        (total - inner_height).saturating_sub(0 /* scroll_offset applied via List */)
    } else {
        0
    };
    let visible: Vec<ListItem> = items.into_iter().skip(skip).collect();
    let list = List::new(visible).block(msg_block);
    f.render_widget(list, chunks[1]);

    // ── Input box ─────────────────────────────────────────────────────────
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            " @name message  or  message (broadcast)  │  ↑↓ scroll  │  Esc quit ",
            Style::default().fg(Color::DarkGray),
        ));

    let input_line = Paragraph::new(format!(" {input}_"))
        .block(input_block)
        .wrap(Wrap { trim: false });
    f.render_widget(input_line, chunks[2]);

    // Place cursor at end of input.
    let cursor_x = chunks[2].x + 1 + input.len() as u16 + 1;
    let cursor_y = chunks[2].y + 1;
    f.set_cursor_position((cursor_x.min(chunks[2].x + chunks[2].width - 2), cursor_y));
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse `@name rest` or just `rest` from an input string.
fn parse_message(input: &str) -> (Option<String>, String) {
    let s = input.trim();
    if let Some(rest) = s.strip_prefix('@') {
        let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next().unwrap_or("").to_string();
        let body = parts.next().unwrap_or("").trim().to_string();
        (Some(name), body)
    } else {
        (None, s.to_string())
    }
}

/// Send `body` to `target_name` (or broadcast if `None`).
/// Uses `send_task` — `description` carries the message text; `payload` is empty
/// for plain text, so the receiver can display `request.description` directly.
/// Returns the number of peers the message was sent to.
async fn send_message(
    handle: &P2pHandle,
    own_name: &str,
    peers: &[(libp2p::PeerId, AgentCard)],
    target_name: Option<&str>,
    body: &str,
) -> usize {
    use sven_p2p::protocol::types::{ContentBlock, TaskRequest};
    let mut count = 0;
    for (peer_id, card) in peers {
        let should_send = match target_name {
            Some(name) => card.name == name,
            None => true,
        };
        if should_send {
            let req = TaskRequest::new(own_name, body, vec![ContentBlock::text(body)]);
            let _ = handle.send_task(*peer_id, req).await;
            count += 1;
        }
    }
    count
}
