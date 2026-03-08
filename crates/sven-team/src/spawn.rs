// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Team lifecycle tools: create_team, spawn_teammate, shutdown_teammate,
//! cleanup_team, list_team.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

use crate::{
    config::{MemberStatus, TeamConfig, TeamConfigStore, TeamMember, TeamRole},
    task::TaskStore,
};

/// Shared handle wrapping the team config for concurrent tool access.
pub type TeamConfigHandle = Arc<Mutex<Option<TeamConfig>>>;

// ── CreateTeamTool ─────────────────────────────────────────────────────────────

/// Initialize a new agent team.
///
/// Creates the team directory structure (`~/.config/sven/teams/{name}/`) and
/// writes an initial `config.json` and an empty `tasks.json`.
pub struct CreateTeamTool {
    /// Name and peer ID of the agent running this tool (becomes the lead).
    pub agent_name: String,
    pub agent_peer_id: String,
    /// Shared config handle so other tools can see the active team.
    pub team_config: TeamConfigHandle,
}

#[async_trait]
impl Tool for CreateTeamTool {
    fn name(&self) -> &str {
        "create_team"
    }

    fn description(&self) -> &str {
        "Initialize a new agent team. You become the team lead. \
         Creates the shared task list and team config. \
         After creating the team, use spawn_teammate to add teammates, \
         then create_task to define the work. \
         Use cleanup_team when all work is done."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Team name (alphanumeric, hyphens OK, e.g. 'auth-refactor')"
                },
                "goal": {
                    "type": "string",
                    "description": "Optional: description of the team's overall objective"
                },
                "max_active": {
                    "type": "integer",
                    "description": "Maximum simultaneous active teammates (default: 8)",
                    "default": 8
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let name = match call.args["name"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: name"),
        };

        // Validate name: alphanumeric + hyphens only.
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return ToolOutput::err(
                &call.id,
                "Team name must be alphanumeric with hyphens or underscores only",
            );
        }

        let goal = call.args["goal"].as_str().map(|s| s.to_string());
        let max_active = call.args["max_active"].as_u64().unwrap_or(8) as usize;

        // Build the config.
        let mut config = TeamConfig::new(&name, &self.agent_peer_id, &self.agent_name);
        config.goal = goal;
        config.max_active = max_active;
        // Set lead peer_id properly.
        if let Some(m) = config.members.first_mut() {
            m.peer_id = self.agent_peer_id.clone();
        }

        // Persist config.
        let cfg_store = match TeamConfigStore::open(&name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to create team dir: {e}")),
        };
        if let Err(e) = cfg_store.save(&config) {
            return ToolOutput::err(&call.id, format!("Failed to save team config: {e}"));
        }

        // Initialize empty task list.
        if let Err(e) = TaskStore::open(&name) {
            return ToolOutput::err(&call.id, format!("Failed to create task list: {e}"));
        }

        // Update in-memory config handle.
        *self.team_config.lock().await = Some(config.clone());

        let goal_note = config
            .goal
            .as_deref()
            .map(|g| format!("\nGoal: {g}"))
            .unwrap_or_default();

        ToolOutput::ok(
            &call.id,
            format!(
                "Team '{name}' created. You are the lead.{goal_note}\n\
                 Next steps:\n\
                 1. Use spawn_teammate to add teammates with specific roles\n\
                 2. Use create_task to define work items\n\
                 3. Assign tasks or let teammates self-claim with claim_task\n\
                 4. Use list_tasks and list_team to monitor progress\n\
                 5. Use cleanup_team when all work is done"
            ),
        )
    }
}

// ── ListTeamTool ──────────────────────────────────────────────────────────────

/// Show all team members with their current status.
pub struct ListTeamTool {
    pub config: TeamConfigHandle,
}

#[async_trait]
impl Tool for ListTeamTool {
    fn name(&self) -> &str {
        "list_team"
    }

    fn description(&self) -> &str {
        "Show all team members with their role, current status, and active task. \
         Use this to monitor who is working on what and whether any teammate needs help."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let guard = self.config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
        };

        let task_store = match TaskStore::open(&config.name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Task store error: {e}")),
        };
        let tasks = task_store.load().unwrap_or_default();
        let (p, i, c, f) = tasks.counts();

        let mut lines = vec![format!(
            "Team '{}' — {} member(s) | tasks: pending={p}, in_progress={i}, completed={c}, failed={f}\n",
            config.name,
            config.members.len()
        )];

        for m in &config.members {
            let role_str = m.role.to_string();
            let status_icon = match m.status {
                MemberStatus::Active => "●",
                MemberStatus::Idle => "○",
                MemberStatus::Closed => "✗",
                MemberStatus::Unknown => "?",
            };

            let task_hint = m
                .current_task_id
                .as_deref()
                .and_then(|id| tasks.get(id))
                .map(|t| format!(" — working on: \"{}\"", t.title))
                .unwrap_or_default();

            let lead_marker = if config.is_lead(&m.peer_id) {
                " [lead]"
            } else {
                ""
            };

            lines.push(format!(
                "{status_icon} **{}** [{}]{lead_marker}{task_hint}\n  peer: {}",
                m.name, role_str, m.peer_id
            ));
        }

        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

// ── CleanupTeamTool ───────────────────────────────────────────────────────────

/// Clean up team resources after all work is done.
pub struct CleanupTeamTool {
    pub config: TeamConfigHandle,
    pub agent_peer_id: String,
}

#[async_trait]
impl Tool for CleanupTeamTool {
    fn name(&self) -> &str {
        "cleanup_team"
    }

    fn description(&self) -> &str {
        "Clean up team resources when all work is done. \
         Only the team lead can run cleanup. \
         Fails if any teammates are still active — shut them down first with shutdown_teammate. \
         This removes the shared task list and team config."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "force": {
                    "type": "boolean",
                    "description": "Force cleanup even if teammates are still active (not recommended)",
                    "default": false
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let force = call.args["force"].as_bool().unwrap_or(false);

        let guard = self.config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return ToolOutput::err(&call.id, "No active team. Nothing to clean up."),
        };

        if !config.is_lead(&self.agent_peer_id) {
            return ToolOutput::err(
                &call.id,
                "Only the team lead can clean up the team. \
                 Teammates should not run cleanup.",
            );
        }

        // Check for active teammates.
        let active: Vec<&TeamMember> = config
            .members
            .iter()
            .filter(|m| m.peer_id != self.agent_peer_id && matches!(m.status, MemberStatus::Active))
            .collect();

        if !active.is_empty() && !force {
            let names: Vec<&str> = active.iter().map(|m| m.name.as_str()).collect();
            return ToolOutput::err(
                &call.id,
                format!(
                    "Cannot clean up: {} teammate(s) still active: {}. \
                     Shut them down first with shutdown_teammate, or pass force=true.",
                    active.len(),
                    names.join(", ")
                ),
            );
        }

        let team_name = config.name.clone();
        drop(guard);

        // Remove team directory.
        let team_dir = crate::task::default_team_dir(&team_name);
        if team_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&team_dir) {
                return ToolOutput::err(&call.id, format!("Failed to remove team directory: {e}"));
            }
        }

        // Clear in-memory handle.
        *self.config.lock().await = None;

        ToolOutput::ok(
            &call.id,
            format!("Team '{team_name}' cleaned up. All team resources have been removed."),
        )
    }
}

// ── RegisterTeammateTool ──────────────────────────────────────────────────────

/// Register a new teammate in the team config (called after a peer joins).
///
/// This is used by the team infrastructure when a peer announces itself
/// as joining the team.  The LLM can also call it explicitly.
pub struct RegisterTeammateTool {
    pub config: TeamConfigHandle,
}

#[async_trait]
impl Tool for RegisterTeammateTool {
    fn name(&self) -> &str {
        "register_teammate"
    }

    fn description(&self) -> &str {
        "Register a new peer as a teammate in the team config. \
         Used when a peer joins the team. \
         Provide the peer's ID, name, and role."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["peer_id", "name"],
            "properties": {
                "peer_id": {
                    "type": "string",
                    "description": "Base58 peer ID of the new teammate"
                },
                "name": {
                    "type": "string",
                    "description": "Human-readable name"
                },
                "role": {
                    "type": "string",
                    "description": "Role: teammate, implementer, reviewer, explorer, tester (default: teammate)",
                    "default": "teammate"
                },
                "model": {
                    "type": "string",
                    "description": "Optional LLM model hint"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let peer_id = match call.args["peer_id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: peer_id"),
        };
        let name = match call.args["name"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: name"),
        };
        let role = match call.args["role"].as_str().unwrap_or("teammate") {
            "implementer" => TeamRole::Implementer,
            "reviewer" => TeamRole::Reviewer,
            "explorer" => TeamRole::Explorer,
            "tester" => TeamRole::Tester,
            "lead" => TeamRole::Lead,
            r => TeamRole::Custom(r.to_string()),
        };
        let model = call.args["model"].as_str().map(|s| s.to_string());

        let guard = self.config.lock().await;
        let team_name = match guard.as_ref() {
            Some(c) => c.name.clone(),
            None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
        };
        drop(guard);

        let cfg_store = match TeamConfigStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Team store error: {e}")),
        };

        let result = cfg_store.modify(|config| {
            // Avoid duplicates.
            if config.members.iter().any(|m| m.peer_id == peer_id) {
                return; // already registered
            }
            config.members.push(TeamMember {
                peer_id: peer_id.clone(),
                name: name.clone(),
                role,
                model,
                status: MemberStatus::Active,
                current_task_id: None,
                joined_at: Utc::now(),
            });
        });

        // Refresh in-memory handle from disk.
        if result.is_ok() {
            if let Ok(Some(updated)) = cfg_store.load() {
                *self.config.lock().await = Some(updated);
            }
        }

        match result {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!("Teammate '{name}' ({peer_id}) registered in team '{team_name}'."),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to register teammate: {e}")),
        }
    }
}

// ── SpawnTeammateTool ─────────────────────────────────────────────────────────

/// Launch a new `sven` process as a teammate and register it in the team.
///
/// The spawned process runs in CI/headless mode and joins the same P2P room
/// as the lead.  The tool registers the teammate in the config so the lead
/// can track its progress.
///
/// The spawned process receives:
/// - `--team-name <name>` — the team to join
/// - `--team-role <role>` — the role to adopt
/// - `--team-lead-peer <peer_id>` — the lead's peer ID
/// - `--room <room>` — gossipsub room to join
/// - `--model <model>` — model override (optional)
/// - `--headless` — run without a TUI
pub struct SpawnTeammateTool {
    pub config: TeamConfigHandle,
    /// Peer ID of this agent (must be lead to spawn teammates).
    pub agent_peer_id: String,
    /// Path to the `sven` binary. Defaults to the current executable.
    pub sven_bin: Option<String>,
}

#[async_trait]
impl Tool for SpawnTeammateTool {
    fn name(&self) -> &str {
        "spawn_teammate"
    }

    fn description(&self) -> &str {
        "Spawn a new sven agent as a teammate in the current team. \
         The teammate runs as a separate process and joins the same P2P room. \
         You must be the team lead to spawn teammates. \
         Use list_team to monitor the new teammate's status. \
         Use shutdown_teammate to stop a running teammate cleanly."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "role"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name for the new teammate (used in team roster and P2P discovery)"
                },
                "role": {
                    "type": "string",
                    "description": "Role: implementer, reviewer, explorer, tester, teammate",
                    "default": "teammate"
                },
                "model": {
                    "type": "string",
                    "description": "Optional LLM model override (e.g. 'anthropic/claude-sonnet-4-6')"
                },
                "task_prompt": {
                    "type": "string",
                    "description": "Initial task description to give the teammate on startup. \
                                    If omitted, the teammate will claim the next available task."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let name = match call.args["name"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: name"),
        };
        let role_str = call.args["role"].as_str().unwrap_or("teammate");
        let model = call.args["model"].as_str().map(|s| s.to_string());
        let task_prompt = call.args["task_prompt"].as_str().map(|s| s.to_string());

        // Guard: must be lead.
        let guard = self.config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
        };
        if !config.is_lead(&self.agent_peer_id) {
            return ToolOutput::err(&call.id, "Only the team lead can spawn teammates.");
        }
        let team_name = config.name.clone();
        let lead_peer_id = self.agent_peer_id.clone();
        drop(guard);

        // Determine binary path.
        let sven_bin = self
            .sven_bin
            .clone()
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "sven".to_string());

        // Build command line arguments for the spawned process.
        let mut args = vec![
            format!("--team-name={team_name}"),
            format!("--team-role={role_str}"),
            format!("--team-lead-peer={lead_peer_id}"),
            format!("--agent-name={name}"),
            "--headless".to_string(),
        ];
        if let Some(ref m) = model {
            args.push(format!("--model={m}"));
        }
        if let Some(ref tp) = task_prompt {
            args.push("--".to_string());
            args.push(tp.clone());
        }

        // Spawn detached.
        let result = std::process::Command::new(&sven_bin)
            .args(&args)
            // Redirect stdio so the spawned process doesn't inherit the TUI.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Detach from process group so SIGINT doesn't kill the child.
            .spawn();

        match result {
            Ok(child) => {
                let pid = child.id();
                ToolOutput::ok(
                    &call.id,
                    format!(
                        "Teammate '{name}' spawned (pid={pid}) with role '{role_str}'.\n\
                         It will join team '{team_name}' and appear in list_team once connected.\n\
                         Use shutdown_teammate to stop it when done."
                    ),
                )
            }
            Err(e) => ToolOutput::err(
                &call.id,
                format!(
                    "Failed to spawn teammate '{name}': {e}\n\
                     Make sure the sven binary is in PATH or set --sven-bin."
                ),
            ),
        }
    }
}

// ── ShutdownTeammateTool ──────────────────────────────────────────────────────

/// Mark a teammate as closed in the team config (graceful shutdown signal).
///
/// The teammate itself is responsible for actually shutting down when it
/// receives this signal (via a gossipsub `TeamLeave` event or by polling the
/// config).  This tool updates the config so the lead's roster reflects the
/// change immediately.
pub struct ShutdownTeammateTool {
    pub config: TeamConfigHandle,
    pub agent_peer_id: String,
}

#[async_trait]
impl Tool for ShutdownTeammateTool {
    fn name(&self) -> &str {
        "shutdown_teammate"
    }

    fn description(&self) -> &str {
        "Signal a teammate to shut down gracefully. \
         Only the team lead can do this. \
         Updates the team config to mark the teammate as closed. \
         The teammate will stop claiming new tasks and finish its current one before exiting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["peer_id"],
            "properties": {
                "peer_id": {
                    "type": "string",
                    "description": "Peer ID of the teammate to shut down"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let target_peer = match call.args["peer_id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: peer_id"),
        };

        let guard = self.config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return ToolOutput::err(&call.id, "No active team."),
        };
        if !config.is_lead(&self.agent_peer_id) {
            return ToolOutput::err(&call.id, "Only the team lead can shut down teammates.");
        }
        let team_name = config.name.clone();
        drop(guard);

        let cfg_store = match TeamConfigStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Team store error: {e}")),
        };

        let result = cfg_store.modify(|config| {
            if let Some(m) = config.members.iter_mut().find(|m| m.peer_id == target_peer) {
                m.status = MemberStatus::Closed;
            }
        });

        if result.is_ok() {
            if let Ok(Some(updated)) = cfg_store.load() {
                *self.config.lock().await = Some(updated);
            }
        }

        match result {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!("Teammate '{target_peer}' marked as closed in team '{team_name}'."),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to update teammate status: {e}")),
        }
    }
}
