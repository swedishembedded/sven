// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::Context;
use sven_config::Config;
use sven_sdk::{
    ApprovalPolicy, Engine, HumanGate, RunConclusion, RunOptions, RunOutcome, SessionEvent, Toolset,
};
use sven_team::{MemberLimits, TokenAllowance};
use sven_tool_api::events::SubagentUpdate;

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

/// Who a teammate process is and what its lead started it with.
pub(crate) struct Teammate {
    pub(crate) name: String,
    pub(crate) team: String,
    pub(crate) role: String,
    /// `--model`: the model the member's runs use instead of the configured one.
    pub(crate) model: Option<String>,
    /// `--append-system-prompt`: the member's instructions from its team
    /// definition.
    pub(crate) instructions: Option<String>,
    /// The prompt it was started with: its first task, from `spawn_teammate`.
    pub(crate) initial_task: Option<String>,
}

/// The configuration one task run uses: the process configuration held to
/// the member's terms from the team config.
fn task_config(base: &Config, limits: &MemberLimits, model: Option<&str>) -> Config {
    let mut config = base.clone();
    if let Some(model) = model {
        config.model = sven_model::resolve_model_from_config(base, model);
    }
    config
        .tools
        .disabled
        .extend(limits.deny_tools.iter().cloned());
    if let Some(rounds) = limits.max_tool_rounds {
        config.agent.max_tool_rounds = rounds;
    }
    config
}

/// The prompt for one claimed task.
fn task_prompt(me: &Teammate, title: &str, description: &str) -> String {
    let instructions = me
        .instructions
        .as_deref()
        .map(|text| format!("## Your instructions\n\n{text}\n\n"))
        .unwrap_or_default();
    format!(
        "You are a teammate named '{}' on team '{}'.\n\
         Complete the following task, then provide a concise summary of \
         what you did and the outcome.\n\n{instructions}## Task: {title}\n\n{description}",
        me.name, me.team
    )
}

/// How a member answers its runs' approval gates, its sub-agents' included:
/// it runs unattended, so it approves any call except one to a tool it is
/// denied, and answers a question with nothing.
fn member_gate(deny_tools: Vec<String>) -> ApprovalPolicy {
    ApprovalPolicy::ask(move |gate| match gate {
        HumanGate::Approval { call, reply_tx, .. } => {
            let denied = call
                .as_ref()
                .is_some_and(|call| deny_tools.contains(&call.name));
            let _ = reply_tx.send(!denied);
        }
        HumanGate::Question { reply_tx, .. } => {
            let _ = reply_tx.send(String::new());
        }
    })
}

/// Runs one task with its output held to `allowance`, and returns how it
/// ended with every token it used - its own and its sub-agents' - whether or
/// not it succeeded.
async fn run_task(
    engine: &Engine,
    prompt: &str,
    allowance: TokenAllowance,
    log_as: &str,
) -> (Result<RunOutcome, sven_sdk::CallError>, u64) {
    let mut agent = engine.agent("agent");
    let mut events = agent.events();
    let log_as = log_as.to_string();
    let meter = tokio::spawn(async move {
        let mut used: u64 = 0;
        loop {
            match events.recv().await {
                Ok(SessionEvent::TokenUsage { input, output, .. }) => {
                    used += u64::from(input) + u64::from(output);
                }
                Ok(SessionEvent::SubagentEvent {
                    update:
                        SubagentUpdate::TokensUsed {
                            input_tokens,
                            output_tokens,
                        },
                    ..
                }) => used += input_tokens + output_tokens,
                Ok(SessionEvent::ToolCallStarted(call)) => {
                    eprintln!("[teammate:{log_as}] tool: {}", call.name);
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        used
    });
    let bounds = match allowance {
        TokenAllowance::Remaining(tokens) => RunOptions::new().max_output_tokens(tokens),
        TokenAllowance::Unlimited | TokenAllowance::Exhausted => RunOptions::new(),
    };
    let outcome = agent.send_with(prompt, bounds).await;
    // Dropping the agent closes its event stream, so the meter has seen
    // every event of the run when it returns.
    drop(agent);
    let used = meter.await.unwrap_or(0);
    (outcome, used)
}

/// Waits before the next look at the team config after `failures`
/// consecutive failed reads; `None` once the config has been unreadable too
/// long to keep trying.
fn config_retry_delay(failures: u32) -> Option<std::time::Duration> {
    const GIVE_UP_AFTER: u32 = 8;
    (failures < GIVE_UP_AFTER).then(|| std::time::Duration::from_secs(5u64 << failures.min(4)))
}

/// Whether `list` already holds this member's initial task: a restart with
/// the same prompt must not put it on the board twice.
fn has_initial_task(list: &sven_team::TaskList, agent_name: &str, text: &str) -> bool {
    list.tasks.iter().any(|task| {
        task.title == INITIAL_TASK_TITLE
            && task.description == text
            && task.assigned_to.as_deref() == Some(agent_name)
    })
}

/// The title of the task a teammate's start prompt becomes.
const INITIAL_TASK_TITLE: &str = "Initial task";

/// Team-member polling loop.
///
/// Registers the process in the team config, then repeatedly:
///   1. Checks for a shutdown signal (status == Closed in the team config).
///   2. Reads its terms from the team config ([`MemberLimits`]): denied
///      tools and the tool-round limit.
///   3. Reserves its share of what is left of the team's token budget, under
///      the team-config lock; a spent budget ends the loop.
///   4. Calls `claim_next(agent_name)` to atomically grab a pending task
///      assigned to this agent (or any unassigned task).
///   5. Runs it as a headless agent held to those terms - its output to the
///      reserved share, its approvals (its sub-agents' included) to its
///      denied tools - and settles the reservation with every token the run
///      and its sub-agents used, whether or not it succeeded.
///   6. Marks the task completed (or failed) with the agent's final response.
///
/// Exits when the shutdown signal is received, the team directory disappears,
/// the team's token budget is spent, or a fatal error prevents task store
/// access; fails when the team config stays unreadable across several
/// retries with growing delays.
pub(crate) async fn run_as_teammate(me: Teammate, config: Arc<Config>) -> anyhow::Result<()> {
    use sven_ci::find_project_root;
    use sven_team::{
        config::{MemberStatus, TeamConfigStore, TeamMember, TeamRole},
        task::TaskStore,
    };

    let agent_name = me.name.clone();
    let team_name = me.team.clone();
    let peer_id = sven_team::teammate_stable_peer_id(&team_name, &agent_name);

    let role = match me.role.as_str() {
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
                model: me.model.clone(),
                status: MemberStatus::Active,
                current_task_id: None,
                joined_at: chrono::Utc::now(),
                pid: Some(our_pid),
                deny_tools: Vec::new(),
            });
        }
    });

    eprintln!("[teammate:{agent_name}] registered in team '{team_name}' peer_id={peer_id}");

    let task_store = TaskStore::open(&team_name)
        .with_context(|| format!("opening task store for '{team_name}'"))?;

    // The task it was started with goes on the board like any other, so the
    // lead sees it and its outcome - once, however often the member restarts.
    if let Some(text) = me.initial_task.as_deref().filter(|t| !t.trim().is_empty()) {
        let recorded = task_store
            .load()
            .is_ok_and(|list| has_initial_task(&list, &agent_name, text));
        if !recorded {
            let id = task_store
                .create_task(INITIAL_TASK_TITLE, text, &agent_name, Vec::new())
                .context("recording the initial task")?;
            task_store
                .assign_task(&id, &agent_name)
                .context("assigning the initial task")?;
        }
    }

    let project_root = find_project_root().ok();

    // ── Polling loop ──────────────────────────────────────────────────────────
    let mut config_failures = 0;
    loop {
        // Check for shutdown signal in the team config, and read this
        // member's terms from it.
        let limits = match cfg_store.load() {
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
                config_failures = 0;
                MemberLimits::for_member(&cfg, &peer_id)
            }
            Ok(None) => {
                // Team config deleted - team was cleaned up.
                eprintln!("[teammate:{agent_name}] team '{team_name}' no longer exists, exiting");
                break;
            }
            Err(e) => {
                eprintln!("[teammate:{agent_name}] config read error: {e}");
                match config_retry_delay(config_failures) {
                    Some(delay) => {
                        config_failures += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    None => {
                        anyhow::bail!(
                            "the config of team '{team_name}' stayed unreadable ({e}); \
                             teammate '{agent_name}' gives up"
                        )
                    }
                }
            }
        };

        // Set this run's share of the team's token budget aside before
        // taking work on.
        let allowance = match cfg_store.reserve_tokens() {
            Ok(TokenAllowance::Exhausted) => {
                eprintln!("[teammate:{agent_name}] the team's token budget is spent, exiting");
                break;
            }
            Ok(allowance) => allowance,
            Err(e) => {
                eprintln!("[teammate:{agent_name}] could not reserve tokens: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        // Try to claim the next available task.
        let claimed = task_store.claim_next(&agent_name);
        if !matches!(claimed, Ok(Some(_))) {
            // No run: hand the reservation back.
            let _ = cfg_store.settle_tokens(allowance, 0);
        }
        match claimed {
            Ok(Some(task)) => {
                eprintln!(
                    "[teammate:{agent_name}] claimed task '{}' (id={})",
                    task.title, task.id
                );

                let prompt = task_prompt(&me, &task.title, &task.description);
                let mut builder = Engine::builder()
                    .config(task_config(&config, &limits, me.model.as_deref()))
                    .toolset(Toolset::coding())
                    .approvals(member_gate(limits.deny_tools.clone()));
                if let Some(root) = &project_root {
                    builder = builder.project_root(root);
                }
                let (run, used) = match builder.build() {
                    Ok(engine) => run_task(&engine, &prompt, allowance, &agent_name).await,
                    Err(e) => (Err(e), 0),
                };
                // Charged whether or not the run succeeded.
                if let Err(e) = cfg_store.settle_tokens(allowance, used) {
                    eprintln!("[teammate:{agent_name}] could not record token usage: {e}");
                }

                match run {
                    Ok(outcome) => {
                        let summary = outcome.reply.trim();
                        if outcome.conclusion == RunConclusion::Success {
                            let _ = task_store.complete_task(&task.id, summary);
                            eprintln!("[teammate:{agent_name}] completed task '{}'", task.title);
                        } else {
                            let reason = format!("{:?}: {summary}", outcome.conclusion);
                            let _ = task_store.fail_task(&task.id, reason);
                            eprintln!(
                                "[teammate:{agent_name}] task '{}' ended: {:?}",
                                task.title, outcome.conclusion
                            );
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_run_is_held_to_the_member_terms() {
        let base = Config::default();
        let limits = MemberLimits {
            deny_tools: vec!["write_file".into(), "shell".into()],
            max_tool_rounds: Some(9),
        };
        let config = task_config(&base, &limits, None);
        assert_eq!(config.tools.disabled, vec!["write_file", "shell"]);
        assert_eq!(config.agent.max_tool_rounds, 9);
        assert_eq!(config.model.name, base.model.name);

        let unlimited = MemberLimits {
            deny_tools: Vec::new(),
            max_tool_rounds: None,
        };
        let config = task_config(&base, &unlimited, Some("mock/other-model"));
        assert_eq!(config.agent.max_tool_rounds, base.agent.max_tool_rounds);
        assert_eq!(config.model.name, "other-model");
    }

    #[test]
    fn a_member_prompt_carries_its_instructions() {
        let me = Teammate {
            name: "rev".into(),
            team: "t".into(),
            role: "reviewer".into(),
            model: None,
            instructions: Some("Do not write code.".into()),
            initial_task: None,
        };
        let prompt = task_prompt(&me, "Review", "the diff");
        assert!(prompt.contains("Do not write code."), "{prompt}");
        assert!(prompt.contains("## Task: Review"), "{prompt}");
    }

    #[tokio::test]
    async fn a_member_approves_what_it_may_use_and_refuses_what_it_is_denied() {
        let ApprovalPolicy::Ask(answer) = member_gate(vec!["shell".into()]) else {
            panic!("a member answers its own gates");
        };
        for (tool, expected) in [("shell", false), ("write_file", true)] {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            answer(HumanGate::Approval {
                capability: sven_sdk::machine::ToolCapability::ExecuteShell,
                prompt: "run it".into(),
                call: Some(sven_sdk::machine::GatedCall {
                    name: tool.into(),
                    args: serde_json::Value::Null,
                }),
                reply_tx,
            });
            assert_eq!(reply_rx.await.unwrap(), expected, "{tool}");
        }
    }

    #[test]
    fn an_unreadable_config_is_retried_with_growing_delays_then_given_up() {
        let delays: Vec<_> = (0..10).map(config_retry_delay).collect();
        assert!(delays[1] > delays[0]);
        assert!(delays.iter().any(Option::is_none), "the member gives up");
        assert!(delays.iter().flatten().all(|d| d.as_secs() <= 80));
    }

    #[test]
    fn a_restart_does_not_repeat_the_initial_task() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = sven_team::TaskStore::open_at(dir.path().join("tasks.json")).unwrap();
        let list = store.load().unwrap();
        assert!(!has_initial_task(&list, "rev", "look"));
        let id = store
            .create_task(INITIAL_TASK_TITLE, "look", "rev", Vec::new())
            .unwrap();
        store.assign_task(&id, "rev").unwrap();
        let list = store.load().unwrap();
        assert!(has_initial_task(&list, "rev", "look"));
        assert!(!has_initial_task(&list, "other", "look"));
    }
}
