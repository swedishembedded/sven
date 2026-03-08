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
    /// When true, create a Git worktree for the teammate so it operates
    /// in an isolated branch.  Requires the working directory to be
    /// inside a Git repository.  Silently skipped when not in a repo.
    pub use_worktree: bool,
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
                },
                "use_worktree": {
                    "type": "boolean",
                    "description": "Create a Git worktree for the teammate so it works in its own \
                                    isolated branch. Requires the current directory to be inside a \
                                    Git repository. Branch: sven/team-{team}/{role}-{name}.",
                    "default": false
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
        let want_worktree = call.args["use_worktree"]
            .as_bool()
            .unwrap_or(self.use_worktree);

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

        // Optionally create a Git worktree for isolated editing.
        let worktree_info = if want_worktree {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match crate::worktree::create_teammate_worktree(&cwd, &team_name, role_str, &name) {
                Ok(guard) => {
                    let branch = guard.branch().to_string();
                    let path = guard.into_path(); // don't auto-remove on drop
                    Some((path, branch))
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not create worktree for teammate '{name}': {e}. \
                         Spawning without isolation."
                    );
                    None
                }
            }
        } else {
            None
        };

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

        // Determine working directory for the spawned process.
        let spawn_cwd = worktree_info
            .as_ref()
            .map(|(p, _)| p.clone())
            .or_else(|| std::env::current_dir().ok());

        // Spawn detached.
        let mut cmd = std::process::Command::new(&sven_bin);
        cmd.args(&args)
            // Redirect stdio so the spawned process doesn't inherit the TUI.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Detach from process group so SIGINT doesn't kill the child.
        if let Some(ref cwd) = spawn_cwd {
            cmd.current_dir(cwd);
        }

        let result = cmd.spawn();

        match result {
            Ok(child) => {
                let pid = child.id();
                let mut msg = format!(
                    "Teammate '{name}' spawned (pid={pid}) with role '{role_str}'.\n\
                     It will join team '{team_name}' and appear in list_team once connected.\n\
                     Use shutdown_teammate to stop it when done."
                );
                if let Some((_, ref branch)) = worktree_info {
                    msg.push_str(&format!(
                        "\nWorktree branch: {branch}\n\
                         Use merge_teammate_branch to merge when done."
                    ));
                }
                ToolOutput::ok(&call.id, msg)
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

// ── MergeTeammateBranchTool ────────────────────────────────────────────────────

/// LLM-callable tool that merges a teammate's branch into the current branch.
///
/// The lead calls this tool after a teammate's work is complete.  Under the
/// hood it runs `git merge --no-ff <branch>` from the repository root so that
/// the merge commit is always created.
pub struct MergeTeammateBranchTool {
    pub config: TeamConfigHandle,
    pub agent_peer_id: String,
}

#[async_trait]
impl Tool for MergeTeammateBranchTool {
    fn name(&self) -> &str {
        "merge_teammate_branch"
    }

    fn description(&self) -> &str {
        "Merge a teammate's Git branch into the current branch. \
         The branch must follow the convention sven/team-{team}/{role}-{name}. \
         Call this after the teammate has completed its work. \
         You must be the team lead to merge branches. \
         A non-fast-forward merge commit is always created."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["branch"],
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch name to merge (e.g. sven/team-auth/reviewer-sec)"
                },
                "message": {
                    "type": "string",
                    "description": "Optional custom commit message for the merge commit"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let branch = match call.args["branch"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: branch"),
        };
        let message = call.args["message"].as_str().map(|s| s.to_string());

        // Guard: must be lead.
        let guard = self.config.lock().await;
        if let Some(cfg) = guard.as_ref() {
            if !cfg.is_lead(&self.agent_peer_id) {
                return ToolOutput::err(&call.id, "Only the team lead can merge branches.");
            }
        }
        drop(guard);

        let cwd = match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("Cannot determine current dir: {e}"))
            }
        };

        let repo_root = match crate::worktree::find_repo_root(&cwd) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::err(
                    &call.id,
                    format!("Not in a Git repo (required for merge): {e}"),
                )
            }
        };

        match crate::worktree::merge_teammate_branch(&repo_root, &branch, message.as_deref()) {
            Ok(msg) => ToolOutput::ok(&call.id, msg),
            Err(e) => ToolOutput::err(
                &call.id,
                format!(
                    "Merge failed for branch {branch:?}: {e}\n\
                     Resolve conflicts manually then commit, or call cleanup_team to abort."
                ),
            ),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::Mutex;

    use sven_tools::{Tool, ToolCall};

    use super::*;

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: String::new(),
            args,
        }
    }

    fn empty_config() -> TeamConfigHandle {
        Arc::new(Mutex::new(None))
    }

    fn config_with_lead(team_name: &str, lead_peer: &str) -> TeamConfigHandle {
        let mut cfg = TeamConfig::new(team_name, lead_peer, "alice");
        cfg.members[0].peer_id = lead_peer.to_string();
        Arc::new(Mutex::new(Some(cfg)))
    }

    // ── Tool names ────────────────────────────────────────────────────────────

    #[test]
    fn tool_names_are_stable() {
        let h = empty_config();
        assert_eq!(
            CreateTeamTool {
                agent_name: "a".into(),
                agent_peer_id: "p".into(),
                team_config: h.clone()
            }
            .name(),
            "create_team"
        );
        assert_eq!(ListTeamTool { config: h.clone() }.name(), "list_team");
        assert_eq!(
            CleanupTeamTool {
                config: h.clone(),
                agent_peer_id: "p".into()
            }
            .name(),
            "cleanup_team"
        );
        assert_eq!(
            RegisterTeammateTool { config: h.clone() }.name(),
            "register_teammate"
        );
        assert_eq!(
            SpawnTeammateTool {
                config: h.clone(),
                agent_peer_id: "p".into(),
                sven_bin: None,
                use_worktree: false,
            }
            .name(),
            "spawn_teammate"
        );
        assert_eq!(
            ShutdownTeammateTool {
                config: h.clone(),
                agent_peer_id: "p".into()
            }
            .name(),
            "shutdown_teammate"
        );
    }

    // ── CreateTeamTool ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_team_missing_name_is_error() {
        let tool = CreateTeamTool {
            agent_name: "alice".into(),
            agent_peer_id: "peer-alice".into(),
            team_config: empty_config(),
        };
        let out = tool.execute(&call("c1", json!({}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn create_team_invalid_name_is_error() {
        let tool = CreateTeamTool {
            agent_name: "alice".into(),
            agent_peer_id: "peer-alice".into(),
            team_config: empty_config(),
        };
        let out = tool
            .execute(&call("c1", json!({ "name": "bad name!" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("alphanumeric"));
    }

    #[tokio::test]
    async fn create_team_sets_in_memory_config() {
        let handle = empty_config();
        let tool = CreateTeamTool {
            agent_name: "alice".into(),
            agent_peer_id: "peer-alice".into(),
            team_config: handle.clone(),
        };
        // Use a unique name to avoid colliding with real ~/.config/sven/teams/.
        let unique = format!("test-{}", uuid::Uuid::new_v4().simple());
        let out = tool.execute(&call("c1", json!({ "name": unique }))).await;
        // Clean up regardless of result to avoid polluting the test env.
        let _ = std::fs::remove_dir_all(crate::task::default_team_dir(&unique));
        assert!(!out.is_error, "create_team failed: {}", out.content);
        let guard = handle.lock().await;
        assert!(
            guard.is_some(),
            "in-memory config should be set after create_team"
        );
    }

    // ── ListTeamTool ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_team_no_active_team_is_error() {
        let tool = ListTeamTool {
            config: empty_config(),
        };
        let out = tool.execute(&call("lt1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn list_team_shows_lead() {
        let cfg = config_with_lead("my-team", "peer-lead");
        let tool = ListTeamTool { config: cfg };
        let out = tool.execute(&call("lt1", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("alice"));
        assert!(out.content.contains("lead"));
    }

    // ── CleanupTeamTool ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn cleanup_team_no_active_team_is_error() {
        let tool = CleanupTeamTool {
            config: empty_config(),
            agent_peer_id: "p".into(),
        };
        let out = tool.execute(&call("cu1", json!({}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn cleanup_team_non_lead_is_error() {
        let cfg = config_with_lead("my-team", "peer-lead");
        let tool = CleanupTeamTool {
            config: cfg,
            agent_peer_id: "peer-bob".into(),
        };
        let out = tool.execute(&call("cu1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("lead"));
    }

    #[tokio::test]
    async fn cleanup_team_with_active_teammate_blocked_without_force() {
        let mut inner = TeamConfig::new("my-team", "peer-lead", "alice");
        inner.members[0].peer_id = "peer-lead".to_string();
        inner.members.push(TeamMember {
            peer_id: "peer-bob".to_string(),
            name: "bob".to_string(),
            role: TeamRole::Teammate,
            model: None,
            status: MemberStatus::Active,
            current_task_id: None,
            joined_at: chrono::Utc::now(),
        });
        let cfg: TeamConfigHandle = Arc::new(Mutex::new(Some(inner)));
        let tool = CleanupTeamTool {
            config: cfg,
            agent_peer_id: "peer-lead".into(),
        };
        let out = tool.execute(&call("cu1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("active"));
    }

    // ── RegisterTeammateTool ──────────────────────────────────────────────────

    #[tokio::test]
    async fn register_teammate_no_active_team_is_error() {
        let tool = RegisterTeammateTool {
            config: empty_config(),
        };
        let out = tool
            .execute(&call("rt1", json!({ "peer_id": "p", "name": "bob" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    // ── SpawnTeammateTool ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_teammate_no_active_team_is_error() {
        let tool = SpawnTeammateTool {
            config: empty_config(),
            agent_peer_id: "p".into(),
            sven_bin: None,
            use_worktree: false,
        };
        let out = tool
            .execute(&call(
                "sp1",
                json!({ "name": "bob", "role": "implementer" }),
            ))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_teammate_non_lead_is_error() {
        let cfg = config_with_lead("my-team", "peer-lead");
        let tool = SpawnTeammateTool {
            config: cfg,
            agent_peer_id: "peer-bob".into(),
            sven_bin: None,
            use_worktree: false,
        };
        let out = tool
            .execute(&call("sp1", json!({ "name": "carol", "role": "reviewer" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("lead"));
    }

    // ── ShutdownTeammateTool ──────────────────────────────────────────────────

    #[tokio::test]
    async fn shutdown_teammate_no_active_team_is_error() {
        let tool = ShutdownTeammateTool {
            config: empty_config(),
            agent_peer_id: "p".into(),
        };
        let out = tool
            .execute(&call("sd1", json!({ "peer_id": "peer-bob" })))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn shutdown_teammate_non_lead_is_error() {
        let cfg = config_with_lead("my-team", "peer-lead");
        let tool = ShutdownTeammateTool {
            config: cfg,
            agent_peer_id: "peer-bob".into(),
        };
        let out = tool
            .execute(&call("sd1", json!({ "peer_id": "peer-carol" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("lead"));
    }
}
