// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Gateway-specific agent tools: `delegate_task`, `list_peers`, and the
//! session/room collaboration tools.
//!
//! These tools give the agent the ability to discover other connected agents
//! and route work to them over the libp2p task protocol.
//!
//! # Security
//!
//! The agent P2P layer authenticates peers via Noise (Ed25519).  Only peers
//! in the `P2pHandle` roster (announced after a successful connection) can be
//! selected by these tools — there is no way to target an arbitrary address.
//!
//! # Usage
//!
//! Both tools are registered in [`crate::agent_builder::build_node_agent`]
//! automatically whenever the node starts.  The LLM will see them in its
//! tool list and can invoke them during any agent turn.
//!
//! Example LLM tool calls:
//!
//! ```json
//! // Discover peers
//! { "tool": "list_peers" }
//!
//! // Delegate work to "backend-agent"
//! { "tool": "delegate_task",
//!   "peer": "backend-agent",
//!   "task": "Run the database migration and report any errors." }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use sven_p2p::{
    protocol::types::{AgentCard, ContentBlock, SessionRole, TaskRequest, TaskStatus},
    store::MessageDirection,
    ConversationStoreHandle, P2pHandle, SessionMessageWire,
};
use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

/// Unified maximum hop depth shared across **all** message channels: task
/// delegation, session messages, and room-post reactive handlers.
///
/// All three channels use this single budget.  When an agent switches between
/// channels — e.g. a session agent that calls `delegate_task`, or a task agent
/// that calls `send_message` — the accumulated depth is carried forward rather
/// than reset, so the combined chain across all protocol transitions cannot
/// exceed this limit.
///
/// With `MAX_HOP_DEPTH = 4`:
/// - Pure task chain: node → A (depth 1) → B (depth 2) → C (depth 3)
///   → D receives depth 4, rejected before the LLM runs.
/// - Pure session chain: depths 1–3 accepted; sending at depth 4 blocked by
///   `SendMessageTool` before any network traffic.
/// - Cross-protocol: a session at depth 3 seeds task agents at depth 3, so
///   they are blocked from delegating on the very first attempt.
///
/// Must equal `MAX_ROOM_POST_DEPTH` in `sven-p2p` (both are 4).
pub const MAX_HOP_DEPTH: u32 = 4;

/// Shared state describing the delegation chain of the task currently
/// being executed by the local agent.
///
/// Each inbound task gets its own fresh `Arc` — there is no shared mutable
/// state between concurrent tasks.  The `DelegateTool` reads this context to
/// enforce cycle and depth limits on any outbound delegation attempt.
#[derive(Debug, Clone, Default)]
pub struct DelegationContext {
    /// Depth at which the current task is executing (0 = local, 1 = once
    /// delegated, …).
    pub depth: u32,
    /// Peer IDs already in the delegation chain, in hop order.
    pub chain: Vec<String>,
}

/// Thread-safe handle to the per-task delegation context.
///
/// Always created fresh per task in [`crate::agent_builder::build_task_agent`].
pub type DelegationContextHandle = Arc<Mutex<Option<DelegationContext>>>;

/// Per-peer session-chain depth tracker shared between [`SendMessageTool`] and
/// [`WaitForMessageTool`].
///
/// Tracking depth per-peer rather than globally solves two problems:
///
/// 1. **Concurrent peer conversations**: a long-lived node agent that converses
///    with peer B and reaches depth 3 would permanently block any subsequent
///    `send_message` to an unrelated peer C if a single global counter were
///    used.  With per-peer tracking, C's chain always starts at
///    `default_depth`.
///
/// 2. **Cross-protocol depth sharing**: `default_depth` is set to the
///    task-delegation depth at which this agent is executing (0 for the
///    interactive node, `task_depth` for per-task agents).  This means any
///    session conversation started by a deep task agent continues the unified
///    hop budget instead of restarting from zero.
///
/// For the **interactive node agent** the tracker must be reset at the start
/// of each new user turn by calling [`SessionDepthTracker::reset_per_turn`].
/// Without the reset, every round-trip with a peer advances the per-peer
/// counter permanently so the depth limit is hit after just two exchanges,
/// blocking legitimate follow-up conversations initiated by the user.
/// Task agents never need this reset because they are created fresh for each
/// inbound task and discarded when the task completes.
#[derive(Debug, Default)]
pub struct SessionDepthTracker {
    /// Baseline depth for any peer not yet present in `per_peer`.
    /// `0` for the interactive node; equals `task_depth` for task agents.
    pub default_depth: u32,
    /// Per-peer depth counters; updated by `WaitForMessageTool` after each
    /// received message.
    pub per_peer: HashMap<String, u32>,
}

impl SessionDepthTracker {
    /// Clear the per-peer depth map at the start of a new interactive user
    /// turn.
    ///
    /// Preserves `default_depth` (which is always 0 for the interactive node)
    /// so that the cross-protocol budget is not affected.  Only the per-peer
    /// high-water marks are cleared — they accumulate within a single turn to
    /// prevent automated ping-pong within that turn, but must not carry over
    /// to the next independent user request.
    pub fn reset_per_turn(&mut self) {
        self.per_peer.clear();
    }
}

pub type SessionDepthHandle = Arc<Mutex<SessionDepthTracker>>;

/// Per-room reactive-post depth tracker shared between [`PostToRoomTool`]
/// instances within a single agent session.
///
/// # Depth semantics
///
/// Three sources contribute to the outgoing depth of a room post, in priority
/// order (highest active value wins):
///
/// 1. **`in_reply_to_depth`** (tool parameter) — the LLM explicitly declares
///    which post it is replying to.  Use `ReadRoomHistoryTool` to obtain the
///    `depth` of the post being answered, then pass it here.  Required for
///    proactive reply chains to propagate depth correctly and prevent loops.
///
/// 2. **`per_room` high-water mark** — the highest depth already sent to this
///    room in the current agent session.  Ensures that multiple replies in the
///    same session continue the chain monotonically rather than restarting from
///    `default_depth` on each call.
///
/// 3. **`default_depth`** — set to the inbound `RoomPost.depth` by the
///    reactive room executor before building the agent, so the very first
///    response from a reactive handler carries `inbound_depth + 1`.  Always
///    `0` for proactive agents such as the node agent and
///    task agents.
///
/// # Proactive vs. reactive posts
///
/// * **Proactive** agents (`default_depth == 0`, no `in_reply_to_depth`): the
///   post is treated as a new, independent topic; outgoing depth = 1.  The
///   `per_room` map is *not* updated, so independent posts never accumulate
///   depth across calls — the node can post status updates all day.
///
/// * **Reply** posts (`in_reply_to_depth` provided): outgoing depth =
///   `in_reply_to_depth + 1`; `per_room` updated for subsequent replies.
///
/// * **Reactive** agents (`default_depth > 0`): any post to this room uses
///   `per_room.get(room).unwrap_or(default_depth) + 1`; `per_room` updated.
#[derive(Debug, Default)]
pub struct RoomDepthTracker {
    /// Baseline depth for any room not yet in `per_room`.
    /// `0` for proactive agents; set to the inbound post's depth for reactive
    /// handlers so the first outgoing post carries `inbound_depth + 1`.
    pub default_depth: u32,
    /// Per-room outgoing-depth high-water mark; updated on each reply or
    /// reactive post.  Independent proactive posts never modify this map.
    pub per_room: HashMap<String, u32>,
}

pub type RoomDepthHandle = Arc<tokio::sync::Mutex<RoomDepthTracker>>;

// ── DelegateTool ─────────────────────────────────────────────────────────────

/// Sends a task to a named or identified peer agent and waits for the result.
///
/// # LAST RESORT — read before using
///
/// This tool exists for cases where a task **genuinely cannot be completed
/// locally** because it requires capabilities or resources that are only
/// available on a specific remote peer.  It is **not** a routing mechanism.
///
/// **Always attempt the task locally first.**  Only call `delegate_task` if
/// you have already tried local execution and confirmed that it is
/// impossible without the remote peer's specific capabilities.
///
/// **Never delegate back to the peer that sent you the current task.**  The
/// runtime enforces this with a hard cycle check — the request will be
/// rejected and you will receive an error.
///
/// # Cycle and depth protection
///
/// Before forwarding, the tool checks two hard conditions enforced in code
/// (not by the LLM):
///
/// 1. **Depth limit** — rejects if the current delegation depth is already
///    at [`MAX_HOP_DEPTH`].
/// 2. **Cycle detection** — rejects if the target peer is already in the
///    delegation chain for this task.
///
/// The outgoing [`TaskRequest`] carries the incremented depth and extended
/// chain so every node in the path applies the same checks.
///
/// # Delegation context
///
/// `delegation_context` is pre-populated at agent construction time with
/// the depth and chain of the inbound task this agent is executing.  Each
/// inbound-task agent gets its own context — there is no shared mutable
/// state between concurrent tasks.
pub struct DelegateTool {
    pub p2p: P2pHandle,
    pub rooms: Vec<String>,
    pub our_card: AgentCard,
    /// Per-task delegation context, pre-populated by `build_task_agent`.
    /// Never shared between concurrent tasks.
    pub delegation_context: DelegationContextHandle,
    /// Optional tool event sender for emitting `DelegateSummary` events.
    pub tool_tx: Option<tokio::sync::mpsc::Sender<sven_tools::events::ToolEvent>>,
}

impl DelegateTool {
    /// Resolve `peer_str` to a `PeerId` by searching all room rosters.
    ///
    /// # Resolution order
    ///
    /// 1. **Exact base58 peer ID string** — unambiguous cryptographic identity.
    ///    Preferred and strongly recommended by the tool description.
    /// 2. **Agent name** — only accepted when the name is **unique** across all
    ///    known peers.  If two peers share the same name the call returns
    ///    `Err(ambiguous_names)` so the caller can report the conflict to the
    ///    LLM and ask it to retry with an explicit peer ID.
    ///
    /// # Security note
    ///
    /// Name-based resolution is intentionally strict: an authorized peer that
    /// registers a name already used by another peer causes an error rather
    /// than silently routing to whichever peer happened to be seen first.
    /// This prevents an attacker from intercepting tasks by cloning a victim's
    /// agent name.
    fn resolve_peer(&self, peer_str: &str) -> Result<libp2p::PeerId, ResolveError> {
        // Collect all known peers once to avoid redundant roster lookups.
        let all = self.p2p.all_peers();

        // ── Pass 1: exact peer-ID match (preferred, unambiguous) ─────────────
        for (peer_id, _) in &all {
            if peer_id.to_base58() == peer_str {
                return Ok(*peer_id);
            }
        }

        // ── Pass 2: name match with duplicate detection ───────────────────────
        let name_matches: Vec<libp2p::PeerId> = all
            .iter()
            .filter(|(_, card)| card.name == peer_str)
            .map(|(pid, _)| *pid)
            .collect();

        match name_matches.len() {
            0 => Err(ResolveError::NotFound),
            1 => Ok(name_matches[0]),
            _ => {
                // Multiple peers share this name — require explicit peer ID to
                // prevent accidental or malicious misdirection.
                let ids: Vec<String> = name_matches.iter().map(|p| p.to_base58()).collect();
                Err(ResolveError::Ambiguous(ids))
            }
        }
    }
}

/// Error returned by [`DelegateTool::resolve_peer`].
#[derive(Debug)]
enum ResolveError {
    NotFound,
    /// Multiple peers share the same name; the Vec contains their peer IDs.
    Ambiguous(Vec<String>),
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "LAST RESORT ONLY: Send a task to a **pre-configured remote peer agent** \
         (a separate sven node listed in swarm.peers) when local execution is genuinely \
         impossible. You MUST attempt the task locally with your own tools first. Only use \
         this tool if (1) you have already tried and failed locally, and (2) the task \
         explicitly requires a capability that only exists on a specific other peer. \
         Never delegate back to the peer that sent you the current task — it will deadlock. \
         Use list_peers first to see available peers and their capabilities. \
         SECURITY: Always use the base58 peer ID (not the name) as the `peer` parameter \
         to avoid name-collision attacks. If you pass a name and multiple peers share it, \
         the call will fail with an ambiguity error. \
         Blocks until the remote agent responds (up to 15 minutes). \
         \n\nNOTE: This does NOT work with locally spawned teammates. \
         Teammates are coordinated through the task system: use `assign_task` to give \
         them work and monitor via `list_tasks` / `list_team`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["peer", "task"],
            "properties": {
                "peer": {
                    "type": "string",
                    "description": "Peer name (e.g. \"backend-agent\") or base58 peer ID returned by list_peers"
                },
                "task": {
                    "type": "string",
                    "description": "Full task description and all context the remote agent needs to complete the work"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let peer_str = match call.args["peer"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: peer"),
        };
        let task_text = match call.args["task"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: task"),
        };

        let peer_id = match self.resolve_peer(&peer_str) {
            Ok(pid) => pid,
            Err(ResolveError::NotFound) => {
                let known: Vec<String> = self
                    .p2p
                    .all_peers()
                    .into_iter()
                    .map(|(pid, card)| format!("{} ({})", card.name, pid.to_base58()))
                    .collect();
                let hint = if known.is_empty() {
                    "No peers currently connected. Use list_peers to check.".to_string()
                } else {
                    format!("Known peers: {}", known.join(", "))
                };
                return ToolOutput::err(&call.id, format!("Peer '{peer_str}' not found. {hint}"));
            }
            Err(ResolveError::Ambiguous(ids)) => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Ambiguous peer name '{peer_str}': multiple peers share this name. \
                         Use the explicit peer ID instead. Matching peer IDs: {}",
                        ids.join(", ")
                    ),
                );
            }
        };

        // ── Cycle and depth checks ────────────────────────────────────────────
        let (outgoing_depth, outgoing_chain) = {
            let ctx = self.delegation_context.lock().await;
            let (current_depth, current_chain) = ctx
                .as_ref()
                .map(|c| (c.depth, c.chain.clone()))
                .unwrap_or((0, vec![]));

            if current_depth >= MAX_HOP_DEPTH {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Cannot delegate: maximum hop depth ({MAX_HOP_DEPTH}) reached. \
                         Execute this sub-task locally instead."
                    ),
                );
            }

            let target_id = peer_id.to_base58();
            if current_chain.contains(&target_id) {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Cannot delegate to '{peer_str}': circular delegation detected. \
                         Peer {target_id} is already in the delegation chain: [{}].",
                        current_chain.join(" → ")
                    ),
                );
            }

            // Build the chain for the outgoing request: append our real libp2p
            // peer ID so the receiver knows we have already processed this task.
            // Guard against the narrow startup window where the OnceLock has not
            // been set yet — an empty string in the chain would corrupt cycle
            // detection on every downstream receiver.
            let our_id = self.p2p.local_peer_id_string();
            if our_id.is_empty() {
                return ToolOutput::err(
                    &call.id,
                    "Cannot delegate: P2P node identity not yet initialised. \
                     Retry in a moment or execute the task locally.",
                );
            }
            let mut new_chain = current_chain;
            new_chain.push(our_id);
            (current_depth + 1, new_chain)
        };

        let room = self
            .rooms
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let task_title_preview: String = task_text
            .lines()
            .next()
            .unwrap_or("delegated task")
            .chars()
            .take(80)
            .collect();
        let request = TaskRequest {
            id: uuid::Uuid::new_v4(),
            originator_room: room,
            description: task_text,
            payload: vec![],
            depth: outgoing_depth,
            chain: outgoing_chain,
            // Signing is performed by NodeState::on_command(SendTask) just
            // before the request is written to the wire.
            hop_public_key: None,
            hop_signature: None,
        };
        let task_id = request.id;

        tracing::info!(
            "Delegating task {} to peer {}",
            task_id,
            peer_id.to_base58()
        );

        let timeout = tokio::time::Duration::from_secs(900);
        match tokio::time::timeout(timeout, self.p2p.send_task(peer_id, request)).await {
            Ok(Ok(response)) => {
                let text = response
                    .result
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");

                let status_label = match &response.status {
                    TaskStatus::Completed => "completed",
                    TaskStatus::Failed { .. } => "failed",
                    TaskStatus::Partial => "partial (incomplete)",
                };

                if let TaskStatus::Failed { reason } = &response.status {
                    return ToolOutput::err(
                        &call.id,
                        format!(
                            "Task failed on agent '{}' after {}ms: {reason}",
                            response.agent.name, response.duration_ms
                        ),
                    );
                }

                // Emit a DelegateSummary tool event so the TUI can show a
                // condensed, collapsible summary of the delegated subtree.
                if let Some(ref tx) = self.tool_tx {
                    let preview = text
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect::<String>();
                    let _ = tx.try_send(sven_tools::events::ToolEvent::DelegateSummary {
                        to_name: response.agent.name.clone(),
                        task_title: task_title_preview.clone(),
                        duration_ms: response.duration_ms,
                        status: status_label.to_string(),
                        result_preview: preview,
                    });
                }

                ToolOutput::ok(
                    &call.id,
                    format!(
                        "Task {status_label} by agent '{}' in {}ms:\n\n{text}",
                        response.agent.name, response.duration_ms
                    ),
                )
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("P2P error sending task: {e}")),
            Err(_) => ToolOutput::err(
                &call.id,
                "Task timed out: remote agent did not respond within 15 minutes",
            ),
        }
    }
}

// ── ListPeersTool ─────────────────────────────────────────────────────────────

/// Lists all currently connected agent peers with their names and capabilities.
///
/// Use this before `delegate_task` to discover what peers are available and
/// what they can do.
pub struct ListPeersTool {
    pub p2p: P2pHandle,
    pub rooms: Vec<String>,
}

#[async_trait]
impl Tool for ListPeersTool {
    fn name(&self) -> &str {
        "list_peers"
    }

    fn description(&self) -> &str {
        "List all currently connected agent peers with their names, capabilities, \
         and peer IDs. Use this to discover available agents before delegating tasks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let all_peers = self.p2p.all_peers();

        if all_peers.is_empty() {
            return ToolOutput::ok(
                &call.id,
                "No agent peers currently connected.\n\
                 Peers are discovered automatically via mDNS on the local network, \
                 or via relay for remote peers.\n\
                 Make sure other agents are running with `sven node start` \
                 and are in the same network or using the same relay.",
            );
        }

        let mut lines = vec![format!("{} peer(s) connected:\n", all_peers.len())];

        for (peer_id, card) in &all_peers {
            lines.push(format!("**{}**", card.name));
            lines.push(format!("  Peer ID:     {}", peer_id.to_base58()));
            if !card.description.is_empty() {
                lines.push(format!("  Description: {}", card.description));
            }
            if !card.capabilities.is_empty() {
                lines.push(format!("  Capabilities: {}", card.capabilities.join(", ")));
            }
            lines.push(format!("  Version: {}", card.version));
            lines.push(String::new());
        }

        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

// ── SendMessageTool ──────────────────────────────────────────────────────────

/// Send a message to a peer agent.
///
/// There is one implicit conversation per peer pair — no session IDs needed.
/// Messages are logged to the local conversation store on both sides.
/// After sending, use `wait_for_message` to receive the reply.
///
/// # Depth propagation
///
/// `session_depth` is a shared counter initialised to `0` for task agents and
/// to the inbound `SessionMessageWire.depth` for session agents.
/// Every message sent by this tool carries `session_depth.load() + 1` on the
/// wire.  When the paired `WaitForMessageTool` delivers a reply it atomically
/// updates `session_depth` to the received message's depth, so the next
/// `send_message` call continues incrementing from the correct depth.
///
/// This ensures task-agent ↔ task-agent ping-pong chains increment the counter
/// on every round trip and hit `MAX_SESSION_DEPTH` just like session-executor
/// auto-response chains, preventing infinite loops regardless of whether the
/// remote end is a session executor or another task agent.
///
/// If sending would produce a depth >= `MAX_SESSION_DEPTH` this tool returns
/// an error to the LLM rather than sending, breaking the chain at the source.
pub struct SendMessageTool {
    pub p2p: P2pHandle,
    /// Shared session-chain depth counter.  Updated by `WaitForMessageTool`
    /// on each received message.  Never shared between concurrent agents —
    /// each agent construction creates its own `Arc<AtomicU32>`.
    pub session_depth: SessionDepthHandle,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a text message to a **pre-configured remote peer agent** (a separate \
         sven node listed in swarm.peers). There is one persistent conversation per \
         peer — like a WhatsApp chat. After sending, call `wait_for_message` with \
         the same peer to receive the reply. Use the base58 peer ID from \
         `list_peers` or a unique agent name. \
         \n\nNOTE: This tool is for inter-node P2P communication ONLY. \
         It does NOT work with locally spawned teammates — teammates are \
         coordinated through the task system: use `assign_task` to give them \
         work and `list_tasks` / `list_team` to monitor progress."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["peer", "text"],
            "properties": {
                "peer": {
                    "type": "string",
                    "description": "Base58 peer ID or unique agent name of the recipient"
                },
                "text": {
                    "type": "string",
                    "description": "Text content of the message"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let peer_str = match call.args["peer"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: peer"),
        };
        let text = match call.args["text"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: text"),
        };

        let peer_id = match resolve_peer_id(&self.p2p, &peer_str) {
            Ok(pid) => pid,
            Err(msg) => return ToolOutput::err(&call.id, msg),
        };

        // Determine next sequence number from the store.
        let peer_id_str = peer_id.to_base58();
        let seq = {
            let store = self.p2p.store().clone();
            let pid = peer_id_str.clone();
            tokio::task::spawn_blocking(move || store.message_count(&pid).unwrap_or(0))
                .await
                .unwrap_or(0)
        };

        // Look up this peer's current depth from the per-peer tracker.
        // Falls back to `default_depth` for peers we haven't talked to yet —
        // for task agents that is the task-delegation depth, ensuring the
        // cross-protocol hop budget is shared rather than restarted from zero.
        let current_depth = {
            let tracker = self.session_depth.lock().await;
            tracker
                .per_peer
                .get(&peer_id_str)
                .copied()
                .unwrap_or(tracker.default_depth)
        };
        let outgoing_depth = current_depth.saturating_add(1);
        if outgoing_depth >= MAX_HOP_DEPTH {
            return ToolOutput::err(
                &call.id,
                format!(
                    "Cannot send message: unified hop-depth limit ({MAX_HOP_DEPTH}) reached. \
                     This conversation chain has already traversed the maximum number of hops \
                     across all protocols — complete the task with the information already available."
                ),
            );
        }

        let msg = SessionMessageWire {
            message_id: Uuid::new_v4(),
            seq,
            timestamp: Utc::now(),
            role: SessionRole::User,
            content: vec![ContentBlock::text(&text)],
            depth: outgoing_depth,
        };

        match self.p2p.send_session_message(peer_id, msg).await {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!(
                    "Message sent to {peer_str} (seq={seq}).\n\
                     Use `wait_for_message {{\"peer\": \"{peer_str}\"}}` to receive the reply."
                ),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to send message: {e}")),
        }
    }
}

// ── WaitForMessageTool ───────────────────────────────────────────────────────

/// Wait for the next reply from a specific peer.
///
/// When a message arrives, the wire `depth` is written back into the shared
/// `session_depth` counter so that the next `send_message` call from the same
/// agent correctly continues the depth chain rather than restarting from zero.
pub struct WaitForMessageTool {
    pub p2p: P2pHandle,
    /// Shared with `SendMessageTool` — updated with the received message's
    /// depth so subsequent sends increment from the correct baseline.
    pub session_depth: SessionDepthHandle,
}

#[async_trait]
impl Tool for WaitForMessageTool {
    fn name(&self) -> &str {
        "wait_for_message"
    }

    fn description(&self) -> &str {
        "Wait for the next message from a specific peer agent. \
         Blocks until the remote peer sends a reply or the timeout elapses. \
         Always specify the same peer you sent to with `send_message`. \
         On timeout the tool returns an error — call it again to keep waiting \
         (the peer waiter slot is released automatically so retrying is safe). \
         Use short timeouts (30–60 s) for interactive use; the node cannot \
         accept new input on this session while the tool is waiting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["peer"],
            "properties": {
                "peer": {
                    "type": "string",
                    "description": "Base58 peer ID or agent name to wait for a message from"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum seconds to wait before returning a timeout error \
                                    (default: 60, max: 300). Prefer short values; call again \
                                    to extend the wait.",
                    "default": 60
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let peer_str = match call.args["peer"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: peer"),
        };
        let peer_id = match resolve_peer_id(&self.p2p, &peer_str) {
            Ok(pid) => pid,
            Err(msg) => return ToolOutput::err(&call.id, msg),
        };
        let timeout_secs = call.args["timeout_secs"].as_u64().unwrap_or(60).min(300);
        let timeout = tokio::time::Duration::from_secs(timeout_secs);

        match self.p2p.wait_for_message(peer_id, timeout).await {
            Ok(record) => {
                // Update the per-peer depth tracker **monotonically** — only
                // advance to the received depth if it is higher than what we
                // already have.  This guards against a race where a session
                // executor's auto-response (typically at a lower depth than an
                // explicit task-agent reply) fires the waiter first and would
                // otherwise reset the tracker downward, allowing a subsequent
                // send_message to carry a depth lower than the chain has already
                // reached.  The invariant is: per_peer[peer] only goes up.
                {
                    let mut tracker = self.session_depth.lock().await;
                    tracker
                        .per_peer
                        .entry(peer_id.to_base58())
                        .and_modify(|d| *d = (*d).max(record.depth))
                        .or_insert(record.depth);
                }

                let text = record
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                ToolOutput::ok(
                    &call.id,
                    format!(
                        "**Reply from {peer_str}** (seq={}, {:?})\n\n{}",
                        record.seq, record.role, text
                    ),
                )
            }
            Err(sven_p2p::P2pError::Timeout) => ToolOutput::err(
                &call.id,
                format!(
                    "No reply from {peer_str} within {timeout_secs}s. \
                     Call wait_for_message again to keep waiting, or move on."
                ),
            ),
            Err(sven_p2p::P2pError::WaiterConflict) => ToolOutput::err(
                &call.id,
                format!(
                    "Cannot wait for {peer_str}: another wait_for_message is already active \
                     for this peer in a different agent. Wait for that agent to finish, or \
                     use a different peer."
                ),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Wait failed: {e}")),
        }
    }
}

// ── SearchConversationTool ───────────────────────────────────────────────────

/// Grep-style regex search over local conversation history.
pub struct SearchConversationTool {
    pub store: ConversationStoreHandle,
}

#[async_trait]
impl Tool for SearchConversationTool {
    fn name(&self) -> &str {
        "search_conversation"
    }

    fn description(&self) -> &str {
        "Grep-style regex search over the local conversation history with peer agents. \
         One conversation per peer is stored locally as JSONL. \
         Use this to find specific information discussed in past exchanges — \
         including messages before the current context break. \
         Supports full Rust regex syntax: anchors, character classes, alternation, etc. \
         Use `(?i)` prefix for case-insensitive matching."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern (grep-style). Use (?i) for case-insensitive. \
                                    Examples: 'auth', '^ERROR', '(?i)boot\\s+fail(ure)?'"
                },
                "peer": {
                    "type": "string",
                    "description": "Optional base58 peer ID or agent name to limit search to one peer"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 20)",
                    "default": 20
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let pattern = match call.args["pattern"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: pattern"),
        };
        // Resolve optional peer name to peer ID string.
        let peer_id_str = call.args["peer"].as_str().map(|s| s.to_string());
        let limit = call.args["limit"].as_u64().unwrap_or(20) as usize;

        let store = Arc::clone(&self.store);
        let pattern_c = pattern.clone();
        let peer_c = peer_id_str.clone();
        let results =
            tokio::task::spawn_blocking(move || store.search(peer_c.as_deref(), &pattern_c, limit))
                .await;

        match results {
            Ok(Ok(records)) if records.is_empty() => {
                ToolOutput::ok(&call.id, format!("No matches for pattern `{pattern}`."))
            }
            Ok(Ok(records)) => {
                let mut lines = vec![format!("{} match(es) for `{pattern}`:\n", records.len())];
                for r in &records {
                    lines.push(format!(
                        "{} {} [{}] seq={}",
                        r.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                        if r.direction == MessageDirection::Outbound {
                            "→"
                        } else {
                            "←"
                        },
                        r.peer_id,
                        r.seq,
                    ));
                    for block in &r.content {
                        if let ContentBlock::Text { text } = block {
                            let preview: String = text.chars().take(400).collect();
                            for line in preview.lines().take(5) {
                                lines.push(format!("  {line}"));
                            }
                        }
                    }
                    lines.push(String::new());
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("Search failed: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("Search task panicked: {e}")),
        }
    }
}

// ── ListConversationsTool ─────────────────────────────────────────────────────

/// List peers that have conversation history stored locally.
pub struct ListConversationsTool {
    pub store: ConversationStoreHandle,
}

#[async_trait]
impl Tool for ListConversationsTool {
    fn name(&self) -> &str {
        "list_conversations"
    }

    fn description(&self) -> &str {
        "List all peer agents that have conversation history stored locally. \
         Returns peer IDs, message counts, and first/last timestamps. \
         Use this to discover who you've talked to before calling \
         `search_conversation` or `send_message`."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let store = Arc::clone(&self.store);
        let summaries = tokio::task::spawn_blocking(move || store.list_peers_with_history()).await;

        match summaries {
            Ok(Ok(list)) if list.is_empty() => {
                ToolOutput::ok(&call.id, "No conversation history recorded yet.")
            }
            Ok(Ok(list)) => {
                let mut lines = vec![format!("{} peer(s) with history:\n", list.len())];
                for s in &list {
                    lines.push(format!("peer: {}", s.peer_id));
                    lines.push(format!("  messages: {}", s.message_count));
                    if let (Some(f), Some(l)) = (s.first_timestamp, s.last_timestamp) {
                        lines.push(format!(
                            "  from {} to {}",
                            f.format("%Y-%m-%d %H:%M UTC"),
                            l.format("%Y-%m-%d %H:%M UTC"),
                        ));
                    }
                    lines.push(String::new());
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("list_conversations failed: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("list_conversations panicked: {e}")),
        }
    }
}

// ── PostToRoomTool ───────────────────────────────────────────────────────────

/// Broadcast a message to all peers currently subscribed to a room.
///
/// Rooms are like Slack channels — presence-based: only peers that are
/// currently connected and subscribed will receive the post.  The message
/// is appended to the local room history file.
///
/// # Reply-chain depth
///
/// To prevent infinite reply loops between agents, every room post carries a
/// hop-depth counter.  When replying to a specific post you MUST pass its
/// `depth` as `in_reply_to_depth` — obtain the value from `read_room_history`
/// output.  The tool then sends at `in_reply_to_depth + 1`.  Posts whose
/// depth reaches `MAX_HOP_DEPTH` are silently dropped by all receivers.
pub struct PostToRoomTool {
    pub p2p: P2pHandle,
    /// Per-room depth tracker for this agent session.
    ///
    /// Initialized by the agent builder:
    /// * `default_depth = 0` for proactive agents (node, task agents).
    /// * `default_depth = inbound_post.depth` for reactive room handlers
    ///   spawned by [`run_room_executor`], so the first outgoing reply carries
    ///   `inbound_depth + 1` without requiring an explicit `in_reply_to_depth`.
    pub room_depth: RoomDepthHandle,
}

#[async_trait]
impl Tool for PostToRoomTool {
    fn name(&self) -> &str {
        "post_to_room"
    }

    fn description(&self) -> &str {
        "Broadcast a message to all agents currently in a room (like a Slack channel). \
         Only agents subscribed to the room at the time of posting will receive it — \
         there is no persistent server-side buffer. The post is logged to the local \
         room history file for later search. \
         IMPORTANT: when replying to a specific room post, you MUST pass its depth \
         value (from read_room_history) as `in_reply_to_depth` to propagate the \
         reply-chain depth correctly and prevent infinite reply loops between agents. \
         Omit `in_reply_to_depth` only for brand-new, independent topic posts."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["room", "text"],
            "properties": {
                "room": {
                    "type": "string",
                    "description": "Name of the room to post to (e.g. 'firmware-team', 'general')"
                },
                "text": {
                    "type": "string",
                    "description": "Text content to broadcast"
                },
                "in_reply_to_depth": {
                    "type": "integer",
                    "description": "Depth of the room post you are replying to. \
                                    Obtain this from read_room_history output (the 'depth=N' field). \
                                    REQUIRED when replying to a specific post — omit only for \
                                    brand-new, independent topic posts."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let room = match call.args["room"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: room"),
        };
        let text = match call.args["text"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: text"),
        };

        // ── Compute outgoing depth ────────────────────────────────────────────
        //
        // Priority (highest applicable source wins):
        //
        //  1. `in_reply_to_depth` (explicit) — the LLM declares the depth of
        //     the post it is replying to.  Required for proactive reply chains.
        //
        //  2. `per_room` high-water mark — ensures multiple replies to the same
        //     room within one agent session advance the counter monotonically.
        //
        //  3. `default_depth` — set to the inbound post's depth by the reactive
        //     executor; first outgoing reply carries `inbound_depth + 1`.
        //
        // Proactive vs. reactive distinction:
        //  * Proactive agents (default_depth == 0) without `in_reply_to_depth`
        //    always post at depth 1 and do NOT update `per_room`.  This lets the
        //    long-lived node agent send unlimited independent status updates
        //    without ever exhausting the hop budget.
        //  * Reply posts or reactive agents DO update `per_room` so subsequent
        //    posts to the same room continue the chain rather than resetting.
        let outgoing_depth = {
            let mut tracker = self.room_depth.lock().await;

            let reply_depth: Option<u32> = call.args["in_reply_to_depth"]
                .as_u64()
                .map(|d| d.min(u64::from(MAX_HOP_DEPTH)) as u32);

            let current = match reply_depth {
                // Explicit in_reply_to_depth: use it directly as the chain base.
                Some(d) => d,
                None => {
                    if tracker.default_depth > 0 {
                        // Reactive handler: start from per_room or default_depth.
                        tracker
                            .per_room
                            .get(&room)
                            .copied()
                            .unwrap_or(tracker.default_depth)
                    } else {
                        // Proactive, independent post (no reply context): always depth 0
                        // so the outgoing post carries depth 1 and per_room is not touched.
                        0
                    }
                }
            };

            let next = current.saturating_add(1);
            if next >= MAX_HOP_DEPTH {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Cannot post to room '{room}': unified hop-depth limit \
                         ({MAX_HOP_DEPTH}) reached. The reply chain has traversed the \
                         maximum number of hops. Do not attempt further replies in this chain."
                    ),
                );
            }

            // Update the per-room high-water mark only for reply chains or reactive
            // agents.  Independent proactive posts (default_depth==0, no in_reply_to_depth)
            // never increment the counter, so the node can post freely.
            if reply_depth.is_some() || tracker.default_depth > 0 {
                tracker
                    .per_room
                    .entry(room.clone())
                    .and_modify(|d| *d = (*d).max(next))
                    .or_insert(next);
            }

            next
        };

        let content = vec![ContentBlock::text(&text)];
        match self.p2p.post_to_room(&room, content, outgoing_depth).await {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!(
                    "Posted to room '{room}' (chain depth={outgoing_depth}). \
                     All currently subscribed peers will receive this."
                ),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to post to room '{room}': {e}")),
        }
    }
}

// ── ReadRoomHistoryTool ──────────────────────────────────────────────────────

/// Read the local room history — posts the node witnessed while subscribed.
pub struct ReadRoomHistoryTool {
    pub store: ConversationStoreHandle,
}

#[async_trait]
impl Tool for ReadRoomHistoryTool {
    fn name(&self) -> &str {
        "read_room_history"
    }

    fn description(&self) -> &str {
        "Read the local history of a room — posts this node received while it was \
         subscribed. Like scrolling up in a Slack channel: you only see what was \
         posted while you were present. Supports full-text search and time filtering."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["room"],
            "properties": {
                "room": {
                    "type": "string",
                    "description": "Room name to read history from"
                },
                "query": {
                    "type": "string",
                    "description": "Optional text filter (case-insensitive)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of posts to return (default: 50)",
                    "default": 50
                },
                "since_hours": {
                    "type": "integer",
                    "description": "Only return posts from the last N hours"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let room = match call.args["room"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: room"),
        };
        let query = call.args["query"].as_str().map(|s| s.to_string());
        let limit = call.args["limit"].as_u64().unwrap_or(50) as usize;
        let since = call.args["since_hours"]
            .as_u64()
            .map(|h| Utc::now() - chrono::Duration::hours(h as i64));

        let store = Arc::clone(&self.store);
        let room_c = room.clone();
        let query_c = query.clone();
        let results = tokio::task::spawn_blocking(move || {
            store.read_room_history(&room_c, since, limit, query_c.as_deref())
        })
        .await;

        match results {
            Ok(Ok(posts)) if posts.is_empty() => {
                ToolOutput::ok(&call.id, format!("No room history found for '{room}'."))
            }
            Ok(Ok(posts)) => {
                let mut lines = vec![format!("{} post(s) from room '{room}':\n", posts.len())];
                for p in &posts {
                    // Include message_id (for deduplication) and depth (for in_reply_to_depth).
                    lines.push(format!(
                        "**{}** @ {} [id={}, depth={}]",
                        p.sender_name,
                        p.timestamp.format("%Y-%m-%d %H:%M UTC"),
                        p.message_id,
                        p.depth,
                    ));
                    for block in &p.content {
                        if let ContentBlock::Text { text } = block {
                            lines.push(format!("  {text}"));
                        }
                    }
                    lines.push(String::new());
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("read_room_history failed: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("read_room_history task panicked: {e}")),
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

fn resolve_peer_id(p2p: &P2pHandle, peer_str: &str) -> Result<libp2p::PeerId, String> {
    let all = p2p.all_peers();

    // Pass 1: exact base58 peer ID match.
    for (pid, _) in &all {
        if pid.to_base58() == peer_str {
            return Ok(*pid);
        }
    }

    // Pass 2: unique name match.
    let name_matches: Vec<libp2p::PeerId> = all
        .iter()
        .filter(|(_, card)| card.name == peer_str)
        .map(|(pid, _)| *pid)
        .collect();

    match name_matches.len() {
        0 => Err(format!(
            "Peer '{peer_str}' not found. Known peers: {}",
            all.iter()
                .map(|(pid, card)| format!("{} ({})", card.name, pid.to_base58()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        1 => Ok(name_matches[0]),
        _ => Err(format!(
            "Ambiguous name '{peer_str}': multiple peers share this name. Use the peer ID. \
             Matches: {}",
            name_matches
                .iter()
                .map(|p| p.to_base58())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

// ── BroadcastAbortTool ────────────────────────────────────────────────────────

/// Broadcast an abort signal to all teammates in the current team.
///
/// This publishes a `BroadcastAbort` gossipsub event so every teammate
/// immediately stops claiming new tasks.  Only the team lead may issue this.
pub struct BroadcastAbortTool {
    pub p2p: P2pHandle,
    pub agent_peer_id: String,
    pub team_config: sven_team::TeamConfigHandle,
}

#[async_trait]
impl sven_tools::Tool for BroadcastAbortTool {
    fn name(&self) -> &str {
        "broadcast_abort"
    }

    fn description(&self) -> &str {
        "Broadcast an abort signal to all teammates in the current team. \
         Only the team lead can do this. \
         Teammates will stop claiming new tasks immediately. \
         Use broadcast_resume to allow them to continue. \
         Use this when the overall goal has changed, or when a critical error occurs."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Human-readable reason for the abort (shown in teammate TUIs)"
                }
            }
        })
    }

    fn default_policy(&self) -> sven_tools::ApprovalPolicy {
        sven_tools::ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> sven_tools::ToolOutput {
        let reason = call.args["reason"]
            .as_str()
            .unwrap_or("lead requested abort")
            .to_string();

        let guard = self.team_config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return sven_tools::ToolOutput::err(&call.id, "No active team."),
        };
        if !config.is_lead(&self.agent_peer_id) {
            return sven_tools::ToolOutput::err(
                &call.id,
                "Only the team lead can broadcast abort.",
            );
        }
        let team_name = config.name.clone();
        drop(guard);

        let event = sven_p2p::protocol::types::TeamEvent::BroadcastAbort {
            team_name: team_name.clone(),
            lead_peer_id: self.agent_peer_id.clone(),
            reason: reason.clone(),
        };

        match self.p2p.publish_team_event(&team_name, event).await {
            Ok(()) => sven_tools::ToolOutput::ok(
                &call.id,
                format!("Abort broadcast to team '{team_name}': {reason}"),
            ),
            Err(e) => {
                sven_tools::ToolOutput::err(&call.id, format!("Failed to publish abort event: {e}"))
            }
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sven_p2p::store::ConversationStore;
    use sven_tools::ToolCall;
    use tempfile::TempDir;

    use super::*;

    fn make_store() -> (TempDir, ConversationStoreHandle) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(ConversationStore::new(dir.path().to_path_buf()));
        (dir, store)
    }

    // ── SearchConversationTool ────────────────────────────────────────────────

    #[test]
    fn search_conversation_tool_name() {
        let (_dir, store) = make_store();
        let tool = SearchConversationTool { store };
        assert_eq!(tool.name(), "search_conversation");
    }

    #[test]
    fn search_conversation_tool_schema_has_pattern_field() {
        let (_dir, store) = make_store();
        let tool = SearchConversationTool { store };
        let schema = tool.parameters_schema();
        let props = schema["properties"]
            .as_object()
            .expect("schema must have properties");
        assert!(
            props.contains_key("pattern"),
            "schema must expose 'pattern' property for regex search"
        );
        let required = schema["required"]
            .as_array()
            .expect("schema must have required array");
        let required_names: Vec<_> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            required_names.contains(&"pattern"),
            "pattern must be required; got: {required_names:?}"
        );
    }

    #[tokio::test]
    async fn search_conversation_tool_missing_pattern_returns_error() {
        let (_dir, store) = make_store();
        let tool = SearchConversationTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "search_conversation".into(),
            args: serde_json::json!({}),
        };
        let out = tool.execute(&call).await;
        assert!(
            out.is_error,
            "missing 'pattern' must produce an error output"
        );
    }

    #[tokio::test]
    async fn search_conversation_tool_invalid_regex_returns_error() {
        let (_dir, store) = make_store();
        let tool = SearchConversationTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "search_conversation".into(),
            args: serde_json::json!({"pattern": "[invalid"}),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error, "invalid regex must produce an error output");
    }

    #[tokio::test]
    async fn search_conversation_tool_empty_store_returns_no_matches() {
        let (_dir, store) = make_store();
        let tool = SearchConversationTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "search_conversation".into(),
            args: serde_json::json!({"pattern": "anything"}),
        };
        let out = tool.execute(&call).await;
        assert!(!out.is_error, "no matches on empty store should not error");
        assert!(
            out.content.contains("No matches"),
            "empty result should say 'No matches'; got: {:?}",
            out.content
        );
    }

    // ── ListConversationsTool ─────────────────────────────────────────────────

    #[test]
    fn list_conversations_tool_name() {
        let (_dir, store) = make_store();
        let tool = ListConversationsTool { store };
        assert_eq!(tool.name(), "list_conversations");
    }

    #[test]
    fn list_conversations_tool_schema_is_object() {
        let (_dir, store) = make_store();
        let tool = ListConversationsTool { store };
        let schema = tool.parameters_schema();
        assert_eq!(
            schema["type"].as_str(),
            Some("object"),
            "parameters_schema must have type=object"
        );
    }

    #[tokio::test]
    async fn list_conversations_tool_empty_store() {
        let (_dir, store) = make_store();
        let tool = ListConversationsTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "list_conversations".into(),
            args: serde_json::json!({}),
        };
        let out = tool.execute(&call).await;
        assert!(!out.is_error, "empty store must not error");
        assert!(
            out.content.contains("No conversation") || out.content.contains("0"),
            "empty store result should say no conversations; got: {:?}",
            out.content
        );
    }

    // ── ReadRoomHistoryTool ───────────────────────────────────────────────────

    #[test]
    fn read_room_history_tool_name() {
        let (_dir, store) = make_store();
        let tool = ReadRoomHistoryTool { store };
        assert_eq!(tool.name(), "read_room_history");
    }

    #[test]
    fn read_room_history_tool_schema_has_required_room() {
        let (_dir, store) = make_store();
        let tool = ReadRoomHistoryTool { store };
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().expect("must have required");
        let names: Vec<_> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"room"),
            "'room' must be required; got: {names:?}"
        );
    }

    #[tokio::test]
    async fn read_room_history_tool_missing_room_returns_error() {
        let (_dir, store) = make_store();
        let tool = ReadRoomHistoryTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "read_room_history".into(),
            args: serde_json::json!({}),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error, "missing 'room' must produce an error");
    }

    #[tokio::test]
    async fn read_room_history_tool_empty_room_returns_no_history() {
        let (_dir, store) = make_store();
        let tool = ReadRoomHistoryTool { store };
        let call = ToolCall {
            id: "tc1".into(),
            name: "read_room_history".into(),
            args: serde_json::json!({"room": "empty-room"}),
        };
        let out = tool.execute(&call).await;
        assert!(!out.is_error, "empty room must not error");
        assert!(
            out.content.contains("No room history"),
            "empty room should say 'No room history'; got: {:?}",
            out.content
        );
    }
}
