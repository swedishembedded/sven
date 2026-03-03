//! Wire-protocol types for sven-p2p.
//!
//! All types derive `Serialize`/`Deserialize` and are encoded as CBOR on the wire.
//! They are deliberately independent of `sven-model` so that the relay binary
//! stays lightweight. The main sven binary provides thin conversion adapters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Agent identity ────────────────────────────────────────────────────────────

/// Describes an agent node: who it is and what it can do.
///
/// Broadcast to every peer on connection; stored in the room roster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentCard {
    /// libp2p `PeerId` serialised as a base58 string.
    pub peer_id: String,
    /// Human-readable name, e.g. `"Alice"` or `"electrical-engineer"`.
    pub name: String,
    /// Free-form description of the agent's expertise.
    pub description: String,
    /// Short capability tags, e.g. `["electrical", "pcb-layout", "rust"]`.
    pub capabilities: Vec<String>,
    /// Crate version string for compatibility checks.
    pub version: String,
}

impl Default for AgentCard {
    fn default() -> Self {
        Self {
            peer_id: String::new(),
            name: "unknown".to_string(),
            description: String::new(),
            capabilities: Vec::new(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

// ── Multimodal content ────────────────────────────────────────────────────────

/// A single content block inside a task payload or response.
///
/// Mirrors `sven-model`'s `ContentPart`/`MessageContent` but carries raw bytes
/// for images so the payload is self-contained over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain UTF-8 text.
    Text { text: String },
    /// Binary image with its MIME type (`image/png`, `image/jpeg`, …).
    Image {
        data: Vec<u8>,
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Arbitrary JSON value (tool call arguments, structured output, etc.).
    Json { value: serde_json::Value },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }

    pub fn json(v: serde_json::Value) -> Self {
        ContentBlock::Json { value: v }
    }
}

// ── Session messaging ─────────────────────────────────────────────────────────

/// The role of the author of a session message.
///
/// Mirrors LLM message roles so that session history can be replayed directly
/// into an agent context window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    /// Message sent by an agent acting as the "user" side of the conversation.
    User,
    /// Message sent by an agent acting as the "assistant" side.
    Assistant,
}

/// A single message sent between two peer agents.
///
/// There is exactly **one** implicit conversation per peer pair — no session
/// IDs are needed.  History is stored locally in a single append-only JSONL
/// file keyed by the remote peer ID.  Breaks in the conversation (gaps longer
/// than a configurable threshold, default 1 hour) divide the log into
/// context windows: the receiving agent loads only messages after the most
/// recent break.  To look before the break, the agent uses the
/// `search_conversation` tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessageWire {
    /// Unique message ID — used for deduplication and ACK correlation.
    pub message_id: Uuid,
    /// Monotonically increasing per-peer sequence number.
    ///
    /// Used to detect dropped or reordered messages.
    pub seq: u64,
    /// When this message was created by the sender.
    pub timestamp: DateTime<Utc>,
    /// Conversational role of the sender in this exchange.
    pub role: SessionRole,
    /// Multimodal message content.
    pub content: Vec<ContentBlock>,
    /// Number of auto-response hops this message has traversed.
    ///
    /// `0` = originated by a human operator or an agent acting on explicit
    /// intent (e.g. `send_message` from a task with no prior session chain).
    /// Each time an agent's session executor auto-responds to an incoming
    /// session message, the outgoing reply carries `depth + 1`.
    ///
    /// The receiving session executor **rejects** messages whose depth has
    /// reached `MAX_SESSION_DEPTH` — it stores the message in history but
    /// does not spawn a new agent or send a reply.  This hard limit is the
    /// primary defence against A↔B echo loops when both agents use
    /// `send_message` inside their session handlers.
    ///
    /// This field is **required** — messages missing it are rejected.
    /// Nodes that do not set this field are incompatible with this version.
    pub depth: u32,
}

/// Maximum reactive-hop depth for room posts.
///
/// `on_gossipsub_message` rejects any post whose `depth >= MAX_ROOM_POST_DEPTH`
/// before it is stored or emitted, breaking gossip flood loops between reactive
/// agents.  Must equal `MAX_HOP_DEPTH` in `sven-node` so the two checks stay
/// in sync.
pub const MAX_ROOM_POST_DEPTH: u32 = 4;

/// A post broadcast to all current subscribers of a named room.
///
/// Room posts are fire-and-forget: there is no ACK.  Each receiving peer logs
/// only what it actually receives while subscribed — exactly the "presence =
/// history" principle of Slack channels.
///
/// Room posts travel over the gossipsub topic `sven/room/<room-name>` so they
/// reach all peers subscribed to that topic at the time of publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomPost {
    /// Unique message ID — used to deduplicate gossipsub re-deliveries.
    pub message_id: Uuid,
    /// Room name — must match the gossipsub topic this message was published to.
    pub room: String,
    /// Sender's libp2p `PeerId` as a base-58 string.
    pub sender_peer_id: String,
    /// Human-readable name of the sender (from `AgentCard`).
    pub sender_name: String,
    /// When this post was created.
    pub timestamp: DateTime<Utc>,
    /// Multimodal content of the post.
    pub content: Vec<ContentBlock>,
    /// Reactive-response hop counter.
    ///
    /// `0` for all posts originating from a tool call.  If a future reactive
    /// room handler auto-responds to a post (e.g. "mention bot"), it must
    /// increment this counter and the handler must refuse to process posts
    /// whose depth has reached the configured limit, breaking gossip floods
    /// between reactive agents.
    pub depth: u32,
}

impl RoomPost {
    /// Build the gossipsub topic string for a given room name.
    pub fn topic_for(room: &str) -> String {
        format!("sven/room/{room}")
    }
}

// ── Task request / response ───────────────────────────────────────────────────

/// Sent from one agent to another to request execution of a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRequest {
    /// Unique identifier — echoed in the `TaskResponse` for correlation.
    pub id: Uuid,
    /// Name of the room from which this request originates.
    pub originator_room: String,
    /// Short description of what the receiving agent should do.
    pub description: String,
    /// Multimodal payload (text prompts, images, JSON context…).
    pub payload: Vec<ContentBlock>,

    /// Number of delegation hops this request has already traversed.
    ///
    /// `0` = request originated directly from a human operator or the local
    /// agent.  Each time an agent forwards the task to a peer via
    /// `delegate_task` this counter is incremented.  The receiving node
    /// **rejects** requests that reach [`MAX_DELEGATION_DEPTH`] before running
    /// the LLM, preventing runaway delegation storms.
    ///
    /// This field is **required** — requests missing it are rejected.
    pub depth: u32,

    /// Ordered list of peer IDs that have already handled this task request,
    /// starting from the originator.
    ///
    /// Each forwarding agent appends its own peer ID before sending.  The
    /// receiver checks whether its own peer ID already appears in this list
    /// and **rejects** the request if so, breaking A→B→A and A→B→C→A cycles
    /// before the LLM ever runs.
    ///
    /// This field is **required** — requests missing it are rejected.
    pub chain: Vec<String>,

    /// Protobuf-encoded Ed25519 public key of the forwarding peer.
    ///
    /// Required for depth > 0 (forwarded) tasks.  The receiver verifies that
    /// this key's derived [`libp2p::PeerId`] matches the Noise-authenticated
    /// sender identity, ensuring the claimed `chain` and `depth` were set by
    /// the actual peer, not by a MITM that modified them in transit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_public_key: Option<Vec<u8>>,

    /// Ed25519 signature over `canonical_hop_bytes(id, depth, chain)`.
    ///
    /// Required for depth > 0 tasks.  Prevents a forwarding peer from
    /// silently manipulating the depth counter or chain entries before
    /// passing the request along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_signature: Option<Vec<u8>>,
}

/// Maximum byte length for a task description field.
/// Longer descriptions are rejected before the LLM runs.
pub const MAX_TASK_DESCRIPTION_BYTES: usize = 32 * 1024; // 32 KiB

/// Maximum total byte size for all payload blocks combined.
pub const MAX_TASK_PAYLOAD_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

/// Build the canonical byte string that a forwarding peer signs and the
/// receiver verifies.
///
/// Format (all lengths big-endian):
/// ```text
/// task_id_bytes (16)
/// depth_u32_be  (4)
/// for each chain entry:
///   entry_len_u16_be (2)
///   entry_bytes      (N)
/// ```
///
/// This encoding is deterministic and unambiguous.
pub fn canonical_hop_bytes(id: &Uuid, depth: u32, chain: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + chain.iter().map(|e| 2 + e.len()).sum::<usize>());
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&depth.to_be_bytes());
    for entry in chain {
        let bytes = entry.as_bytes();
        let len = bytes.len() as u16;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

impl TaskRequest {
    pub fn new(
        room: impl Into<String>,
        description: impl Into<String>,
        payload: Vec<ContentBlock>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            originator_room: room.into(),
            description: description.into(),
            payload,
            depth: 0,
            chain: Vec::new(),
            hop_public_key: None,
            hop_signature: None,
        }
    }
}

/// Sent back to the requester after the task is complete (or has failed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResponse {
    /// Matches `TaskRequest::id`.
    pub request_id: Uuid,
    /// Identity of the agent that handled the request.
    pub agent: AgentCard,
    /// The result — may contain multiple content blocks (text + images etc.).
    pub result: Vec<ContentBlock>,
    /// Completion status.
    pub status: TaskStatus,
    /// Wall-clock duration of task execution in milliseconds.
    pub duration_ms: u64,
}

/// Outcome of a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskStatus {
    Completed,
    Failed {
        reason: String,
    },
    /// Task produced partial results but did not fully complete.
    Partial,
}

// ── Wire envelope ─────────────────────────────────────────────────────────────

/// Top-level request sent from one peer to another.
///
/// `Task` is the single work unit of the protocol — it covers everything from
/// a simple one-line chat message to a richly multimodal agent invocation.
/// For a plain text message, set `description` to the message text and leave
/// `payload` empty; the receiver displays `description` and `Ack`s immediately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum P2pRequest {
    /// Announce this agent's `AgentCard` to the remote peer (sent on connect).
    Announce(AgentCard),
    /// Send a task (or plain text message) to the remote peer.
    Task(TaskRequest),
    /// Periodic keep-alive probe sent to all roster peers.
    ///
    /// Receiver responds with [`P2pResponse::Ack`].  Opening this substream
    /// resets the swarm's idle-connection timer, preventing the connection from
    /// being closed between tasks.
    Heartbeat,
    /// Send a message to a peer.  There is one implicit conversation per peer
    /// pair — no session IDs needed.  The receiver appends it to the local
    /// conversation log keyed by the sender's peer ID and responds with
    /// [`P2pResponse::SessionAck`].  Any reply arrives as a subsequent inbound
    /// `SessionMessage` from the remote peer.
    SessionMessage(SessionMessageWire),
}

/// Top-level response sent back in reply to a `P2pRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum P2pResponse {
    /// Generic acknowledgement (for `Announce` and `Heartbeat`).
    Ack,
    /// Result of a task execution (for `Task`).
    TaskResult(TaskResponse),
    /// Delivery acknowledgement for a `SessionMessage`.
    /// Echoes `message_id` so the sender can confirm delivery.
    SessionAck { message_id: Uuid },
}

// ── Logging ───────────────────────────────────────────────────────────────────

/// A captured tracing log record forwarded through the log channel so the TUI
/// can display P2P internals without them going to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub level: String,
    pub target: String,
    pub message: String,
}
