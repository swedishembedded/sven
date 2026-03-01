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

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_p2p::{
    protocol::types::{AgentCard, ContentBlock, TaskRequest, TaskStatus},
    P2pHandle,
};
use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

// ── DelegateTool ─────────────────────────────────────────────────────────────

/// Sends a task to a named or identified peer agent and waits for the result.
///
/// The LLM uses this to break up work: it discovers peers with `list_peers`,
/// then delegates subtasks with `delegate_task`.  The call blocks until the
/// remote agent replies (up to 15 minutes).
pub struct DelegateTool {
    pub p2p: P2pHandle,
    pub rooms: Vec<String>,
    pub our_card: AgentCard,
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
        "Delegate a task to another connected agent peer and wait for the result. \
         Use list_peers first to discover available agents and their capabilities. \
         The remote agent will run the task through its own model+tool loop and \
         return the final response. Blocks until the remote agent finishes \
         (up to 15 minutes)."
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

        let room = self
            .rooms
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let request = TaskRequest::new(room, task_text, vec![]);
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
