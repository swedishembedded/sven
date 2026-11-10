// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running several coding agents
// in parallel on one repository, with every change integrated or reported,
// for its clients. If your team needs expertise in parallel agent workflows
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Runs a validated graph: ready tasks start as writing children, at most
//! `max_parallel` at once, each only after everything it depends on has been
//! integrated. This is deterministic code; the model is not consulted.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

use super::graph::{Limits, Plan, TaskSpec};

/// What became of one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The child finished and its change is in the parent's tree.
    Integrated,
    /// The child finished but its change overlaps work already in the tree;
    /// nothing was applied.
    Conflict,
    /// The child did not finish; nothing was applied.
    Failed,
    /// Not started: a task it depends on did not integrate.
    Skipped,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integrated => "integrated",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// What a child run came to, as the spawner read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOutcome {
    pub status: Status,
    pub files_changed: Vec<String>,
    /// The branch holding a conflicting or failed child's work.
    pub branch: Option<String>,
    /// The child's own report, or why it failed.
    pub report: String,
    /// Provider-reported input and output tokens, when known.
    pub tokens: Option<(u64, u64)>,
}

/// The record of one task in a graph's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    pub id: String,
    pub title: String,
    pub outcome: ChildOutcome,
    pub wall: Duration,
}

/// Runs one task as a writing child in a worktree of its own and integrates
/// its change. The production one is the `task` tool; tests script it.
#[async_trait]
pub trait Spawner: Send + Sync {
    async fn run(&self, call_id: &str, task: &TaskSpec, brief: String) -> ChildOutcome;
}

fn skipped(task: &TaskSpec, blocker: &str, blocker_status: Status) -> TaskReport {
    TaskReport {
        id: task.id.clone(),
        title: task.title.clone(),
        outcome: ChildOutcome {
            status: Status::Skipped,
            files_changed: Vec::new(),
            branch: None,
            report: format!(
                "not started: task `{blocker}` it depends on is {}",
                blocker_status.as_str()
            ),
            tokens: None,
        },
        wall: Duration::ZERO,
    }
}

async fn run_one(
    spawner: &dyn Spawner,
    call_id: &str,
    task: &TaskSpec,
    brief: String,
    time_limit: Option<Duration>,
) -> TaskReport {
    let started = Instant::now();
    let outcome = match time_limit {
        None => spawner.run(call_id, task, brief).await,
        // A child past its time is stopped and its change never integrated,
        // however it was about to finish.
        Some(limit) => match tokio::time::timeout(limit, spawner.run(call_id, task, brief)).await {
            Ok(outcome) => outcome,
            Err(_) => ChildOutcome {
                status: Status::Failed,
                files_changed: Vec::new(),
                branch: None,
                report: format!(
                    "ran past its {}s time budget and was stopped; nothing was integrated",
                    limit.as_secs()
                ),
                tokens: None,
            },
        },
    };
    TaskReport {
        id: task.id.clone(),
        title: task.title.clone(),
        outcome,
        wall: started.elapsed(),
    }
}

/// Runs every task of `plan` (already validated) and reports each, in plan
/// order. `call_id` is the `plan_tasks` call the children belong to;
/// `brief_of` builds a child's prompt; `time_limit` bounds one child from
/// start to integration.
pub async fn run_graph(
    plan: &Plan,
    limits: &Limits,
    spawner: &dyn Spawner,
    call_id: &str,
    brief_of: &(dyn Fn(&TaskSpec) -> String + Sync),
    time_limit: Option<Duration>,
) -> Vec<TaskReport> {
    let tasks = &plan.tasks;
    let position = |id: &str| tasks.iter().position(|t| t.id == id).expect("validated");
    let mut reports: Vec<Option<TaskReport>> = vec![None; tasks.len()];
    let mut started = vec![false; tasks.len()];
    let mut running: FuturesUnordered<BoxFuture<'_, (usize, TaskReport)>> = FuturesUnordered::new();
    loop {
        let mut progressed = true;
        while progressed {
            progressed = false;
            for i in 0..tasks.len() {
                let task = &tasks[i];
                if started[i] {
                    continue;
                }
                let deps: Vec<usize> = task.depends_on.iter().map(|d| position(d)).collect();
                let blocker = deps.iter().find_map(|&d| {
                    reports[d]
                        .as_ref()
                        .filter(|r| r.outcome.status != Status::Integrated)
                });
                if let Some(blocker) = blocker {
                    reports[i] = Some(skipped(task, &blocker.id, blocker.outcome.status));
                    started[i] = true;
                    progressed = true;
                } else if deps.iter().all(|&d| reports[d].is_some())
                    && running.len() < limits.max_parallel
                {
                    started[i] = true;
                    let brief = brief_of(task);
                    running.push(Box::pin(async move {
                        (i, run_one(spawner, call_id, task, brief, time_limit).await)
                    }));
                }
            }
        }
        match running.next().await {
            Some((i, report)) => reports[i] = Some(report),
            None => break,
        }
    }
    reports
        .into_iter()
        .map(|r| r.expect("a validated graph leaves no task unrun"))
        .collect()
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
