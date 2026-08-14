// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::Context;

use crate::cli::TeamCommands;

// ── Team command handler ──────────────────────────────────────────────────────

pub(crate) fn run_team_command(cmd: &TeamCommands) -> anyhow::Result<()> {
    match cmd {
        TeamCommands::List => sven_team::cli::cmd_list(),

        TeamCommands::Status { name } => sven_team::cli::cmd_status(name),

        TeamCommands::Create {
            name,
            goal,
            max_active,
            token_budget,
        } => sven_team::cli::cmd_create(name, goal.as_deref(), *max_active, *token_budget),

        TeamCommands::Start {
            file,
            sven_bin,
            dry_run,
        } => sven_team::cli::cmd_start(file, sven_bin.as_deref(), *dry_run),

        TeamCommands::Cleanup { name, force } => sven_team::cli::cmd_cleanup(name, *force),

        TeamCommands::Definitions => {
            let project_root =
                sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
            sven_team::cli::cmd_definitions(&project_root)
        }

        TeamCommands::Init { name, goal } => {
            let project_root =
                sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
            sven_team::cli::cmd_init(&project_root, name, goal.as_deref())
        }

        TeamCommands::Watch {
            name,
            interval,
            timeout,
        } => sven_team::cli::cmd_watch(name, *interval, *timeout),
    }
}

// ── Teammate runner ───────────────────────────────────────────────────────────

/// Team-member polling loop.
///
/// Registers the process in the team config, then repeatedly:
///   1. Checks for a shutdown signal (status == Closed in the team config).
///   2. Calls `claim_next(agent_name)` to atomically grab a pending task
///      assigned to this agent (or any unassigned task).
///   3. Runs `CiRunner` with the task description as the prompt.
///   4. Marks the task completed (or failed) with the agent's final response.
///
/// Exits when the shutdown signal is received, the team directory disappears,
/// or a fatal error prevents task store access.
pub(crate) async fn run_as_teammate(
    agent_name: String,
    team_name: String,
    role_str: String,
    config: Arc<sven_config::Config>,
) -> anyhow::Result<()> {
    use sven_ci::{find_project_root, CiOptions, CiRunner, OutputFormat};
    use sven_config::AgentMode;
    use sven_team::{
        config::{MemberStatus, TeamConfigStore, TeamMember, TeamRole},
        task::TaskStore,
    };

    let peer_id = sven_team::teammate_stable_peer_id(&team_name, &agent_name);

    let role = match role_str.as_str() {
        "implementer" => TeamRole::Implementer,
        "reviewer" => TeamRole::Reviewer,
        "explorer" => TeamRole::Explorer,
        "tester" => TeamRole::Tester,
        _ => TeamRole::Teammate,
    };

    // ── Register in team config ───────────────────────────────────────────────
    let cfg_store = TeamConfigStore::open(&team_name)
        .with_context(|| format!("opening team config for '{team_name}'"))?;

    let our_pid = std::process::id();
    let _ = cfg_store.modify(|cfg| {
        // Idempotent: update PID on re-registration; add on first start.
        if let Some(m) = cfg.members.iter_mut().find(|m| m.peer_id == peer_id) {
            m.pid = Some(our_pid);
            m.status = MemberStatus::Active;
        } else {
            cfg.members.push(TeamMember {
                peer_id: peer_id.clone(),
                name: agent_name.clone(),
                role: role.clone(),
                model: None,
                status: MemberStatus::Active,
                current_task_id: None,
                joined_at: chrono::Utc::now(),
                pid: Some(our_pid),
            });
        }
    });

    eprintln!("[teammate:{agent_name}] registered in team '{team_name}' peer_id={peer_id}");

    let task_store = TaskStore::open(&team_name)
        .with_context(|| format!("opening task store for '{team_name}'"))?;

    let project_root = find_project_root().ok();

    // ── Polling loop ──────────────────────────────────────────────────────────
    loop {
        // Check for shutdown signal in the team config.
        match cfg_store.load() {
            Ok(Some(cfg)) => {
                if let Some(me) = cfg.members.iter().find(|m| m.peer_id == peer_id) {
                    if matches!(me.status, MemberStatus::Closed) {
                        eprintln!("[teammate:{agent_name}] shutdown signal received, exiting");
                        break;
                    }
                } else {
                    // We were removed from the config - treat as shutdown.
                    eprintln!("[teammate:{agent_name}] removed from team config, exiting");
                    break;
                }
            }
            Ok(None) => {
                // Team config deleted - team was cleaned up.
                eprintln!("[teammate:{agent_name}] team '{team_name}' no longer exists, exiting");
                break;
            }
            Err(e) => {
                eprintln!("[teammate:{agent_name}] config read error: {e}");
            }
        }

        // Try to claim the next available task.
        match task_store.claim_next(&agent_name) {
            Ok(Some(task)) => {
                eprintln!(
                    "[teammate:{agent_name}] claimed task '{}' (id={})",
                    task.title, task.id
                );

                // Build a prompt that includes the task context.
                let prompt = format!(
                    "You are a teammate named '{agent_name}' on team '{team_name}'.\n\
                     Complete the following task, then provide a concise summary of \
                     what you did and the outcome.\n\n\
                     ## Task: {}\n\n{}",
                    task.title, task.description
                );

                // Use a temp file to capture the agent's final response (summary).
                let summary_path = std::env::temp_dir().join(format!("sven-task-{}.txt", task.id));

                let ci_opts = CiOptions {
                    mode: AgentMode::Agent,
                    model_override: None,
                    input: String::new(),
                    extra_prompt: Some(prompt),
                    input_from_file: false,
                    project_root: project_root.clone(),
                    output_format: OutputFormat::Compact,
                    artifacts_dir: None,
                    vars: Default::default(),
                    step_timeout_secs: None,
                    run_timeout_secs: None,
                    dry_run: false,
                    output_last_message: Some(summary_path.clone()),
                    system_prompt_file: None,
                    append_system_prompt: None,
                    no_system: false,
                    no_tools: false,
                    trace_level: 0,
                    load_trace: None,
                    output_trace: None,
                    rerun_toolcalls: false,
                    regen_system_prompt: false,
                    max_tokens_budget: None,
                    attachments: Vec::new(),
                };

                let run_result = CiRunner::new(config.clone()).run(ci_opts).await;

                let summary = if summary_path.exists() {
                    std::fs::read_to_string(&summary_path)
                        .unwrap_or_else(|_| "(no summary)".to_string())
                } else {
                    "(no summary)".to_string()
                };
                let _ = std::fs::remove_file(&summary_path);

                match run_result {
                    Ok(()) => {
                        let _ = task_store.complete_task(&task.id, summary.trim());
                        eprintln!("[teammate:{agent_name}] completed task '{}'", task.title);
                    }
                    Err(e) => {
                        let _ = task_store.fail_task(&task.id, format!("Agent error: {e}"));
                        eprintln!("[teammate:{agent_name}] task '{}' failed: {e}", task.title);
                    }
                }
            }
            Ok(None) => {
                // No tasks available - wait before polling again.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                eprintln!("[teammate:{agent_name}] task store error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }

    // Mark ourselves as closed on clean exit.
    let _ = cfg_store.modify(|cfg| {
        if let Some(m) = cfg.members.iter_mut().find(|m| m.peer_id == peer_id) {
            m.status = MemberStatus::Closed;
        }
    });

    Ok(())
}
