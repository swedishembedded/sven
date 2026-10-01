// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tools the team lead uses on a finished teammate's work: merging its
//! branch and reading its log.

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tool_api::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

use crate::spawn::TeamConfigHandle;

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

// ── ReadTeammateLogTool ────────────────────────────────────────────────────────

/// Read the tail of a spawned teammate's log file.
///
/// The log is written by `SpawnTeammateTool` to
/// `~/.config/sven/teams/{team}/{name}.log`.  This tool lets the lead check
/// whether a spawned process started correctly, see its latest output, and
/// diagnose startup failures - without needing to know the log path.
pub struct ReadTeammateLogTool {
    pub config: TeamConfigHandle,
}

#[async_trait]
impl Tool for ReadTeammateLogTool {
    fn name(&self) -> &str {
        "read_teammate_log"
    }

    fn description(&self) -> &str {
        "Read the last N lines from a spawned teammate's log file. \
         Use this to check whether a teammate started correctly, to see its latest \
         output and progress, or to diagnose why it exited unexpectedly. \
         Also reports whether the teammate process is currently running. \
         Always call this before concluding a teammate is broken or needs re-spawning."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the teammate whose log to read"
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of lines to show from the end of the log (default: 50)",
                    "default": 50
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
        let max_lines = call.args["lines"].as_u64().unwrap_or(50) as usize;

        let guard = self.config.lock().await;
        let config = match guard.as_ref() {
            Some(c) => c,
            None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
        };
        let team_name = config.name.clone();

        // Find the member by name to get its PID.
        let (pid, process_status) = match config
            .members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(&name))
        {
            Some(m) => match m.pid {
                Some(p) => {
                    let alive = crate::config::is_process_alive(p);
                    (
                        Some(p),
                        if alive {
                            format!("running (pid={p})")
                        } else {
                            format!("exited (pid={p} - process no longer running)")
                        },
                    )
                }
                None => (
                    None,
                    "unknown (no PID recorded - manually registered peer?)".to_string(),
                ),
            },
            None => (
                None,
                "not found in team roster - check spelling or use list_team".to_string(),
            ),
        };
        drop(guard);

        let log_path = crate::task::default_team_dir(&team_name).join(format!("{name}.log"));

        if !log_path.exists() {
            return ToolOutput::ok(
                &call.id,
                format!(
                    "Teammate '{name}' - process status: {process_status}\n\
                     No log file found at: {}\n\
                     The teammate may not have been spawned yet, or was spawned with a different name.",
                    log_path.display()
                ),
            );
        }

        let content = match std::fs::read_to_string(&log_path) {
            Ok(s) => s,
            Err(e) => {
                return ToolOutput::err(
                    &call.id,
                    format!("Failed to read log at {}: {e}", log_path.display()),
                )
            }
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let tail: Vec<&str> = if all_lines.len() > max_lines {
            all_lines[all_lines.len() - max_lines..].to_vec()
        } else {
            all_lines.clone()
        };

        let log_section = if tail.is_empty() {
            "(log file is empty - the process may have started but produced no output yet)"
                .to_string()
        } else {
            tail.join("\n")
        };

        let _ = pid; // pid used above for status string
        ToolOutput::ok(
            &call.id,
            format!(
                "Teammate '{name}' - process status: {process_status}\n\
                 Log: {} ({} lines total, showing last {max_lines})\n\
                 {}\n{}",
                log_path.display(),
                all_lines.len(),
                "─".repeat(60),
                log_section,
            ),
        )
    }
}
