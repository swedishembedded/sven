// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! CLI implementation for `sven team` subcommands.
//!
//! These functions are called from the main binary's command dispatch.

use std::path::{Path, PathBuf};

use crate::{
    config::{MemberStatus, TeamConfig, TeamConfigStore, TeamRole},
    definition::{discover_team_definitions, TeamDefinition},
    task::{TaskStatus, TaskStore},
};

// ── sven team list ────────────────────────────────────────────────────────────

/// List all known teams (those with a config directory under `~/.config/sven/teams/`).
pub fn cmd_list() -> anyhow::Result<()> {
    let teams_dir = base_teams_dir();
    if !teams_dir.exists() {
        println!("No teams found.");
        println!("Create a team with:  sven team create --name <NAME> --goal <GOAL>");
        println!(
            "Or start from a definition file:  sven team start --file .sven/teams/review.yaml"
        );
        return Ok(());
    }

    let entries: Vec<_> = std::fs::read_dir(&teams_dir)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", teams_dir.display()))?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();

    if entries.is_empty() {
        println!("No teams found.");
        return Ok(());
    }

    println!("{:<24}  {:<8}  {:<8}  GOAL", "TEAM", "MEMBERS", "ACTIVE");
    println!("{}", "-".repeat(72));

    for entry in entries {
        let team_name = entry.file_name().to_string_lossy().to_string();
        let store = TeamConfigStore::open(&team_name);
        match store.map(|s| s.load()) {
            Ok(Ok(Some(cfg))) => {
                let active = cfg
                    .members
                    .iter()
                    .filter(|m| matches!(m.status, MemberStatus::Active))
                    .count();
                let goal = cfg
                    .goal
                    .as_deref()
                    .unwrap_or("-")
                    .chars()
                    .take(40)
                    .collect::<String>();
                println!(
                    "{:<24}  {:<8}  {:<8}  {}",
                    team_name,
                    cfg.members.len(),
                    active,
                    goal
                );
            }
            _ => {
                println!("{:<24}  (config unreadable)", team_name);
            }
        }
    }
    Ok(())
}

// ── sven team status ──────────────────────────────────────────────────────────

/// Print detailed status for a specific team.
pub fn cmd_status(team_name: &str) -> anyhow::Result<()> {
    let store = TeamConfigStore::open(team_name)
        .map_err(|e| anyhow::anyhow!("Cannot open team {team_name:?}: {e}"))?;
    let cfg = store
        .load()?
        .ok_or_else(|| anyhow::anyhow!("Team {team_name:?} not found"))?;

    println!("Team: {}", cfg.name);
    if let Some(ref goal) = cfg.goal {
        println!("Goal: {goal}");
    }
    println!("Lead: {}", cfg.lead_peer_id);
    println!("Created: {}", cfg.created_at.format("%Y-%m-%d %H:%M UTC"));
    if cfg.token_budget > 0 {
        let pct = (cfg.tokens_used * 100) / cfg.token_budget;
        println!(
            "Token budget: {}/{} ({pct}% used)",
            cfg.tokens_used, cfg.token_budget
        );
    }
    println!();
    println!("Members ({}):", cfg.members.len());
    println!(
        "  {:<24}  {:<12}  {:<8}  CURRENT TASK",
        "NAME", "ROLE", "STATUS"
    );
    println!("  {}", "-".repeat(72));
    for m in &cfg.members {
        let task = m
            .current_task_id
            .as_deref()
            .unwrap_or("-")
            .chars()
            .take(36)
            .collect::<String>();
        println!(
            "  {:<24}  {:<12}  {:<8}  {}",
            m.name,
            m.role.to_string(),
            m.status.to_string(),
            task
        );
    }

    // Show task summary
    let task_store = TaskStore::open(team_name);
    if let Ok(ts) = task_store {
        if let Ok(list) = ts.load() {
            println!();
            let pending = list
                .tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::Pending))
                .count();
            let in_progress = list
                .tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::InProgress { .. }))
                .count();
            let completed = list
                .tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::Completed { .. }))
                .count();
            let failed = list
                .tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::Failed { .. }))
                .count();
            println!(
                "Tasks: {} pending, {} in-progress, {} completed, {} failed",
                pending, in_progress, completed, failed
            );
        }
    }

    Ok(())
}

// ── sven team create ──────────────────────────────────────────────────────────

/// Create a new team and print its configuration.
pub fn cmd_create(
    name: &str,
    goal: Option<&str>,
    max_active: usize,
    token_budget: u64,
) -> anyhow::Result<()> {
    // Validate name.
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("Team name must be alphanumeric with hyphens or underscores only");
    }

    let store = TeamConfigStore::open(name)
        .map_err(|e| anyhow::anyhow!("Cannot create team {name:?}: {e}"))?;

    if store.load()?.is_some() {
        anyhow::bail!("Team {name:?} already exists. Use 'sven team status {name}' to inspect it.");
    }

    let mut cfg = TeamConfig::new(name, "cli", "sven-cli");
    cfg.goal = goal.map(|s| s.to_string());
    cfg.max_active = max_active;
    cfg.token_budget = token_budget;
    // CLI-created teams start with a placeholder lead peer ID.
    if let Some(lead) = cfg.members.first_mut() {
        lead.peer_id = "cli".to_string();
    }

    store.save(&cfg)?;

    println!("Created team '{name}'.");
    if let Some(g) = goal {
        println!("Goal: {g}");
    }
    println!(
        "Add members or run 'sven team start --name {name}' to launch agents from a definition file."
    );
    Ok(())
}

// ── sven team start ───────────────────────────────────────────────────────────

/// Start a team from a definition file, spawning one agent process per member.
pub fn cmd_start(file: &Path, sven_bin: Option<&str>, dry_run: bool) -> anyhow::Result<()> {
    let def = TeamDefinition::from_file(file)?;

    if def.members.is_empty() {
        anyhow::bail!("Team definition has no members — nothing to start.");
    }

    println!(
        "Starting team '{}' ({} members)",
        def.name,
        def.members.len()
    );
    if let Some(ref goal) = def.goal {
        println!("Goal: {goal}");
    }

    let sven = sven_bin
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "sven".to_string());

    for member in &def.members {
        let mut args = vec![
            format!("--team-name={}", def.name),
            format!("--team-role={}", member.role),
            format!("--agent-name={}", member.name),
            "--headless".to_string(),
        ];
        if let Some(ref m) = member.model {
            args.push(format!("--model={m}"));
        }
        if let Some(ref instr) = member.instructions {
            args.push(format!("--append-system-prompt={}", instr));
        }

        if dry_run {
            println!("  [dry-run] would spawn: {} {}", sven, args.join(" "));
            continue;
        }

        match std::process::Command::new(&sven)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => println!(
                "  Spawned '{}' ({}) — pid={}",
                member.name,
                member.role,
                child.id()
            ),
            Err(e) => eprintln!("  [warn] Failed to spawn '{}': {e}", member.name),
        }
    }

    if !dry_run {
        println!(
            "\nAll members launched. Use 'sven team status {}' to monitor.",
            def.name
        );
    }
    Ok(())
}

// ── sven team cleanup ─────────────────────────────────────────────────────────

/// Remove a team's configuration directory.
pub fn cmd_cleanup(team_name: &str, force: bool) -> anyhow::Result<()> {
    let dir = default_team_dir(team_name);
    if !dir.exists() {
        println!("Team '{team_name}' not found — nothing to clean up.");
        return Ok(());
    }

    if !force {
        // Show status first.
        let _ = cmd_status(team_name);
        println!();
        println!("Remove all team data for '{team_name}'? (pass --force to skip this check)");
        // In non-interactive mode just return without deleting.
        println!("Nothing removed. Pass --force to actually delete.");
        return Ok(());
    }

    std::fs::remove_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("Failed to remove {}: {e}", dir.display()))?;
    println!("Team '{team_name}' removed.");
    Ok(())
}

// ── sven team definitions ─────────────────────────────────────────────────────

/// List team definition files found in the project.
pub fn cmd_definitions(project_root: &Path) -> anyhow::Result<()> {
    let defs = discover_team_definitions(project_root)?;
    if defs.is_empty() {
        println!("No team definition files found in .sven/teams/");
        println!("Create one with: sven team init --name <NAME>");
        return Ok(());
    }

    println!("{:<24}  {:<32}  MEMBERS  FILE", "NAME", "GOAL");
    println!("{}", "-".repeat(80));
    for (path, def) in &defs {
        let goal = def
            .goal
            .as_deref()
            .unwrap_or("-")
            .chars()
            .take(30)
            .collect::<String>();
        let rel = path
            .strip_prefix(project_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        println!(
            "{:<24}  {:<32}  {:<7}  {}",
            def.name,
            goal,
            def.members.len(),
            rel
        );
    }
    Ok(())
}

/// Write a starter team definition file.
pub fn cmd_init(project_root: &Path, name: &str, goal: Option<&str>) -> anyhow::Result<()> {
    let teams_dir = project_root.join(".sven").join("teams");
    std::fs::create_dir_all(&teams_dir)?;

    let path = teams_dir.join(format!("{name}.yaml"));
    if path.exists() {
        anyhow::bail!(
            "Team definition {:?} already exists. Edit it directly.",
            path
        );
    }

    let def = TeamDefinition {
        name: name.to_string(),
        goal: goal.map(|s| s.to_string()),
        members: vec![
            crate::definition::TeamMemberDef {
                role: TeamRole::Implementer,
                name: format!("{name}-implementer"),
                model: None,
                instructions: Some("Implement the assigned tasks.".to_string()),
                deny_tools: Vec::new(),
            },
            crate::definition::TeamMemberDef {
                role: TeamRole::Reviewer,
                name: format!("{name}-reviewer"),
                model: None,
                instructions: Some(
                    "Review the work done by implementers. Do not write code.".to_string(),
                ),
                deny_tools: vec![
                    "write_file".to_string(),
                    "edit_file".to_string(),
                    "delete_file".to_string(),
                ],
            },
        ],
        max_active: 4,
        token_budget: 0,
        max_iterations: 0,
    };

    def.to_file(&path)?;
    println!("Created {}", path.display());
    println!("Edit it to customise roles and models, then run:");
    println!("  sven team start --file {}", path.display());
    Ok(())
}

// ── sven team watch ───────────────────────────────────────────────────────────

/// Watch a team's live status by polling its config and task list.
///
/// This is a simple polling dashboard that prints status lines to stderr
/// (diagnostics) and emits structured event lines to stdout for downstream
/// processing.  For full P2P event streaming, run a `sven node` and subscribe
/// via the P2P gossipsub room.
pub fn cmd_watch(team_name: &str, interval_secs: u64, timeout_secs: u64) -> anyhow::Result<()> {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let interval = Duration::from_secs(interval_secs.max(1));
    let mut prev_task_counts: HashMap<String, usize> = HashMap::new();
    let mut prev_member_statuses: HashMap<String, String> = HashMap::new();

    eprintln!("[sven:watch] Watching team '{team_name}' (Ctrl-C to stop)");

    loop {
        if timeout_secs > 0 && start.elapsed() > Duration::from_secs(timeout_secs) {
            eprintln!("[sven:watch] Timeout reached. Stopping.");
            break;
        }

        let timestamp = chrono::Utc::now().format("%H:%M:%S").to_string();

        // Load team config.
        if let Ok(store) = TeamConfigStore::open(team_name) {
            if let Ok(Some(cfg)) = store.load() {
                // Detect member status changes.
                for m in &cfg.members {
                    let key = m.peer_id.clone();
                    let status_str = m.status.to_string();
                    if prev_member_statuses
                        .get(&key)
                        .map(|s| s != &status_str)
                        .unwrap_or(true)
                    {
                        eprintln!(
                            "[sven:watch:{timestamp}] member={} role={} status={}",
                            m.name, m.role, status_str
                        );
                        println!(
                            "{{\"ts\":\"{timestamp}\",\"kind\":\"member_status\",\"name\":\"{}\",\"role\":\"{}\",\"status\":\"{}\"}}",
                            m.name, m.role, status_str
                        );
                        prev_member_statuses.insert(key, status_str);
                    }
                }

                // Token budget check.
                if cfg.token_budget > 0 {
                    let pct = cfg.tokens_used * 100 / cfg.token_budget;
                    if pct >= 80 {
                        eprintln!(
                            "[sven:watch:{timestamp}] token_budget={}/{} ({pct}%)",
                            cfg.tokens_used, cfg.token_budget
                        );
                        if pct >= 90 {
                            println!(
                                "{{\"ts\":\"{timestamp}\",\"kind\":\"token_budget_warning\",\"team\":\"{team_name}\",\"percent_used\":{pct}}}"
                            );
                        }
                    }
                }
            }
        }

        // Load task list.
        if let Ok(ts) = TaskStore::open(team_name) {
            if let Ok(list) = ts.load() {
                let completed = list
                    .tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::Completed { .. }))
                    .count();
                let in_progress = list
                    .tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::InProgress { .. }))
                    .count();
                let pending = list
                    .tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::Pending))
                    .count();
                let total = list.tasks.len();

                let new_counts: HashMap<String, usize> = [
                    ("completed".to_string(), completed),
                    ("in_progress".to_string(), in_progress),
                    ("pending".to_string(), pending),
                ]
                .into();

                if prev_task_counts != new_counts {
                    eprintln!(
                        "[sven:watch:{timestamp}] tasks: {completed}/{total} completed, {in_progress} active, {pending} pending"
                    );
                    println!(
                        "{{\"ts\":\"{timestamp}\",\"kind\":\"task_summary\",\"team\":\"{team_name}\",\"completed\":{completed},\"in_progress\":{in_progress},\"pending\":{pending},\"total\":{total}}}"
                    );
                    prev_task_counts = new_counts;
                }

                // All done?
                if pending == 0 && in_progress == 0 && total > 0 && completed == total {
                    eprintln!("[sven:watch:{timestamp}] All tasks completed. Team finished.");
                    println!(
                        "{{\"ts\":\"{timestamp}\",\"kind\":\"team_complete\",\"team\":\"{team_name}\"}}"
                    );
                    break;
                }
            }
        }

        std::thread::sleep(interval);
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn base_teams_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("sven")
        .join("teams")
}

fn default_team_dir(team_name: &str) -> PathBuf {
    base_teams_dir().join(team_name)
}
