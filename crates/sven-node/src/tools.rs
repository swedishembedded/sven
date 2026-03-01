// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Gateway-specific agent tools: `delegate_task` and `list_peers`.
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
//! Both tools are registered in [`crate::agent_builder::build_gateway_agent`]
//! automatically whenever the gateway starts.  The LLM will see them in its
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

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_p2p::{
    protocol::types::{AgentCard, ContentBlock, TaskRequest, TaskStatus},
    P2pHandle,
};
use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

/// Maximum number of delegation hops before a task is rejected.
///
/// With `MAX_DELEGATION_DEPTH = 3` the longest possible chain is:
/// human → A → B → C → D where D only executes locally.
/// Any further forwarding attempt is refused before the LLM runs.
pub const MAX_DELEGATION_DEPTH: u32 = 3;

/// Shared state describing the delegation chain of the task currently
/// being executed by the local agent.
///
/// Set by [`crate::node::execute_inbound_task`] just before the agent
/// session starts, and cleared after it finishes.  The [`DelegateTool`]
/// reads it to enforce cycle and depth limits on any outbound delegation.
///
/// # Concurrency note
///
/// This is a single shared slot.  If two inbound tasks are executing
/// concurrently the slot holds whichever context was set last, which may
/// be stale for the other task.  The **hard depth check in
/// `execute_inbound_task`** (which fires *before* the LLM) provides the
/// correctness guarantee in all cases; the context here adds an early
/// cycle-detection path that is best-effort under high concurrency.
#[derive(Debug, Clone, Default)]
pub struct DelegationContext {
    /// Depth at which the current task is executing (0 = local, 1 = once
    /// delegated, …).
    pub depth: u32,
    /// Peer IDs already in the delegation chain, in hop order.
    pub chain: Vec<String>,
}

/// Thread-safe handle to the shared delegation context.
pub type DelegationContextHandle = Arc<Mutex<Option<DelegationContext>>>;

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
///    at [`MAX_DELEGATION_DEPTH`].
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
}

impl DelegateTool {
    /// Resolve `peer_str` to a PeerId by searching all room rosters.
    ///
    /// Matches (in order):
    /// 1. Exact base58 peer ID string.
    /// 2. Agent `name` field (case-sensitive).
    fn resolve_peer(&self, peer_str: &str) -> Option<libp2p::PeerId> {
        // Check all rooms first via room_peers(), then fall back to all_peers().
        for room in &self.rooms {
            for (peer_id, card) in self.p2p.room_peers(room) {
                if peer_id.to_base58() == peer_str || card.name == peer_str {
                    return Some(peer_id);
                }
            }
        }
        // Also check peers discovered outside configured rooms (e.g. mDNS).
        for (peer_id, card) in self.p2p.all_peers() {
            if peer_id.to_base58() == peer_str || card.name == peer_str {
                return Some(peer_id);
            }
        }
        None
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "LAST RESORT ONLY: Send a task to a peer agent when local execution is genuinely \
         impossible. You MUST attempt the task locally with your own tools first. Only use \
         this tool if (1) you have already tried and failed locally, and (2) the task \
         explicitly requires a capability that only exists on a specific other peer. \
         Never delegate back to the peer that sent you the current task — it will deadlock. \
         Use list_peers first to see available peers and their capabilities. \
         Blocks until the remote agent responds (up to 15 minutes)."
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
            Some(pid) => pid,
            None => {
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
        };

        // ── Cycle and depth checks ────────────────────────────────────────────
        let (outgoing_depth, outgoing_chain) = {
            let ctx = self.delegation_context.lock().await;
            let (current_depth, current_chain) = ctx
                .as_ref()
                .map(|c| (c.depth, c.chain.clone()))
                .unwrap_or((0, vec![]));

            if current_depth >= MAX_DELEGATION_DEPTH {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Cannot delegate: maximum delegation depth ({MAX_DELEGATION_DEPTH}) \
                         reached. Execute this sub-task locally instead."
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

            // Build the chain for the outgoing request: append our own peer ID
            // so the receiver knows we have already processed this task.
            let mut new_chain = current_chain;
            new_chain.push(self.our_card.peer_id.clone());
            (current_depth + 1, new_chain)
        };

        let room = self
            .rooms
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let request = TaskRequest {
            id: uuid::Uuid::new_v4(),
            originator_room: room,
            description: task_text,
            payload: vec![],
            depth: outgoing_depth,
            chain: outgoing_chain,
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
                 Make sure other agents are running with `sven gateway start` \
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
