// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: how a graph is run. Children are scripted; time is real but short.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use super::super::graph::{Limits, Plan, TaskSpec};
use super::{run_graph, ChildOutcome, Spawner, Status};

fn spec(id: &str, deps: &[&str]) -> TaskSpec {
    TaskSpec {
        id: id.into(),
        title: id.into(),
        prompt: format!("do {id}"),
        owns: vec![format!("{id}.py")],
        acceptance: String::new(),
        depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
    }
}

fn plan(tasks: Vec<TaskSpec>) -> Plan {
    Plan {
        tasks,
        verify_command: None,
    }
}

fn limits(max_parallel: usize) -> Limits {
    Limits {
        max_parallel,
        max_fanout: 8,
        max_tasks: 24,
        max_graphs: 3,
    }
}

fn outcome(status: Status, report: &str) -> ChildOutcome {
    ChildOutcome {
        status,
        files_changed: Vec::new(),
        branch: None,
        report: report.into(),
        tokens: None,
    }
}

/// Scripts each task's status and records when children start and finish.
#[derive(Default)]
struct Script {
    status: HashMap<String, Status>,
    hold: Duration,
    live: AtomicUsize,
    most_live: AtomicUsize,
    log: Mutex<Vec<String>>,
}

#[async_trait]
impl Spawner for Script {
    async fn run(&self, _call: &str, task: &TaskSpec, _brief: String) -> ChildOutcome {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_live.fetch_max(live, Ordering::SeqCst);
        self.log.lock().unwrap().push(format!("start {}", task.id));
        tokio::time::sleep(self.hold).await;
        self.log.lock().unwrap().push(format!("end {}", task.id));
        self.live.fetch_sub(1, Ordering::SeqCst);
        let status = self
            .status
            .get(&task.id)
            .copied()
            .unwrap_or(Status::Integrated);
        outcome(status, &format!("{} reported", task.id))
    }
}

async fn run(script: &Script, plan: &Plan, parallel: usize) -> Vec<(String, Status)> {
    run_graph(
        plan,
        &limits(parallel),
        script,
        "call",
        &|t| t.prompt.clone(),
        None,
    )
    .await
    .into_iter()
    .map(|r| (r.id, r.outcome.status))
    .collect()
}

#[tokio::test]
async fn independent_tasks_run_at_the_same_time_and_all_integrate() {
    let script = Script {
        hold: Duration::from_millis(50),
        ..Script::default()
    };
    let got = run(&script, &plan(vec![spec("a", &[]), spec("b", &[])]), 4).await;
    assert_eq!(
        got,
        vec![
            ("a".into(), Status::Integrated),
            ("b".into(), Status::Integrated)
        ]
    );
    assert_eq!(script.most_live.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn no_more_than_max_parallel_run_at_once() {
    let script = Script {
        hold: Duration::from_millis(20),
        ..Script::default()
    };
    let tasks = (0..5).map(|i| spec(&format!("t{i}"), &[])).collect();
    run(&script, &plan(tasks), 2).await;
    assert_eq!(script.most_live.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_task_starts_only_after_its_prerequisite_has_integrated() {
    let script = Script {
        hold: Duration::from_millis(20),
        ..Script::default()
    };
    run(&script, &plan(vec![spec("b", &["a"]), spec("a", &[])]), 4).await;
    assert_eq!(
        *script.log.lock().unwrap(),
        ["start a", "end a", "start b", "end b"]
    );
}

#[tokio::test]
async fn a_task_that_does_not_integrate_skips_everything_downstream_of_it() {
    let script = Script {
        status: HashMap::from([("a".to_string(), Status::Conflict)]),
        ..Script::default()
    };
    let graph = plan(vec![
        spec("a", &[]),
        spec("b", &["a"]),
        spec("c", &["b"]),
        spec("d", &[]),
    ]);
    let got = run(&script, &graph, 4).await;
    assert_eq!(
        got,
        vec![
            ("a".into(), Status::Conflict),
            ("b".into(), Status::Skipped),
            ("c".into(), Status::Skipped),
            ("d".into(), Status::Integrated),
        ]
    );
    let log = script.log.lock().unwrap().join(",");
    assert!(
        !log.contains("start b") && !log.contains("start c"),
        "{log}"
    );
}

#[tokio::test]
async fn a_child_past_its_time_is_failed_and_not_integrated() {
    let script = Script {
        hold: Duration::from_secs(30),
        ..Script::default()
    };
    let reports = run_graph(
        &plan(vec![spec("a", &[]), spec("b", &["a"])]),
        &limits(2),
        &script,
        "call",
        &|t| t.prompt.clone(),
        Some(Duration::from_millis(30)),
    )
    .await;
    assert_eq!(reports[0].outcome.status, Status::Failed);
    assert!(reports[0].outcome.report.contains("time budget"));
    assert_eq!(reports[1].outcome.status, Status::Skipped);
}
