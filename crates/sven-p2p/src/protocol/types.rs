//! Wire-protocol types for sven-p2p.
//!
//! All types derive `Serialize`/`Deserialize` and are encoded as CBOR on the wire.
//! They are deliberately independent of `sven-model` so that the relay binary
//! stays lightweight. The main sven binary provides thin conversion adapters.

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
    Failed { reason: String },
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
}

/// Top-level response sent back in reply to a `P2pRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum P2pResponse {
    /// Generic acknowledgement (for `Announce`).
    Ack,
    /// Result of a task execution (for `Task`).
    TaskResult(TaskResponse),
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
