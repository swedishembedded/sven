// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: `plan_tasks` end to end over real git worktrees. The children are
//! scripted (the `ChildRunner` seam), everything from there on is the real
//! `task` tool: worktrees, three-way integration, kept branches.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tool_api::ApprovalPolicy;

use super::graph::Limits;
use super::spawner::TaskToolSpawner;
use super::{register, PlanTasksTool};
use crate::config::{Config, Orchestration};
use crate::task_tool::runner::ChildRunner;
use crate::task_tool::tests::make_task;
use crate::task_tool::{SpawnArgs, TaskTool};

fn limits() -> Limits {
    Limits {
        max_parallel: 4,
        max_fanout: 8,
        max_tasks: 24,
        max_graphs: 2,
    }
}

type Edit = Box<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// A child that applies the edit scripted for the task named in its brief,
/// after the delay scripted for it, and says what it saw.
#[derive(Default)]
struct Children {
    edits: HashMap<String, Edit>,
    delays: HashMap<String, Duration>,
    /// Tasks that must all have started before any of them may finish.
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
}

#[async_trait]
impl ChildRunner for Children {
    async fn run(&self, args: SpawnArgs) -> ToolOutput {
        let id = args
            .prompt
            .split('`')
            .nth(1)
            .expect("the brief names its task")
            .to_string();
        if let Some(barrier) = &self.rendezvous {
            tokio::time::timeout(Duration::from_secs(20), barrier.wait())
                .await
                .expect("the children did not run at the same time");
        }
        if let Some(delay) = self.delays.get(&id) {
            tokio::time::sleep(*delay).await;
        }
        match self.edits.get(&id).expect("an edit per task")(&args.workdir) {
            Ok(()) => ToolOutput::ok(&args.call_id, format!("{id} finished")),
            Err(e) => ToolOutput::err(&args.call_id, e),
        }
    }
}

/// A verification command whose outcome is fixed.
struct Verifier {
    passes: bool,
    output: &'static str,
}

#[async_trait]
impl Tool for Verifier {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        ""
    }
    fn parameters_schema(&self) -> Value {
        json!({})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
        sven_tool_api::ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        assert!(call.args["shell_command"].is_string());
        if self.passes {
            ToolOutput::ok(&call.id, self.output)
        } else {
            ToolOutput::err(&call.id, self.output)
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().unwrap();
        let p = repo.path();
        git(p, &["init", "--quiet", "-b", "main"]);
        git(p, &["config", "user.name", "t"]);
        git(p, &["config", "user.email", "t@t"]);
        std::fs::write(p.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "--quiet", "-m", "init"]);
        Self {
            repo,
            state: tempfile::tempdir().unwrap(),
        }
    }

    fn tool(&self, children: Children, verifier: Verifier) -> PlanTasksTool {
        let task: TaskTool = make_task()
            .with_scope(sven_tool_api::PathScope::confined(self.repo.path()).unwrap())
            .with_worktree_root(self.state.path().to_path_buf())
            .with_runner(Arc::new(children));
        PlanTasksTool::new(
            Arc::new(TaskToolSpawner {
                task: Arc::new(task),
            }),
            Arc::new(verifier),
            limits(),
            None,
        )
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.repo.path().join(file)).unwrap_or_default()
    }

    fn worktrees_left(&self) -> bool {
        std::fs::read_dir(self.state.path())
            .unwrap()
            .next()
            .is_some()
    }
}

fn writes(file: &'static str, text: &'static str) -> Edit {
    Box::new(move |dir| std::fs::write(dir.join(file), text).map_err(|e| e.to_string()))
}

fn task(id: &str, owns: &str, deps: &[&str]) -> Value {
    json!({"id": id, "prompt": format!("build {id}"), "owns": [owns],
           "acceptance": "done", "depends_on": deps})
}

fn passing() -> Verifier {
    Verifier {
        passes: true,
        output: "all passed",
    }
}

async fn submit(tool: &PlanTasksTool, args: Value) -> (ToolOutput, Value) {
    let out = tool
        .execute(&ToolCall {
            id: "plan-1".into(),
            name: "plan_tasks".into(),
            args,
        })
        .await;
    let parsed = serde_json::from_str(&out.content).unwrap_or(Value::Null);
    (out, parsed)
}

fn status_of(result: &Value, id: &str) -> String {
    result["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("no task {id} in {result}"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn two_independent_tasks_run_concurrently_and_both_integrate() {
    let fx = Fixture::new();
    let children = Children {
        edits: HashMap::from([
            ("a".into(), writes("a.py", "A\n")),
            ("b".into(), writes("b.py", "B\n")),
        ]),
        // Neither child can finish unless the other has started.
        rendezvous: Some(Arc::new(tokio::sync::Barrier::new(2))),
        ..Children::default()
    };
    let tool = fx.tool(children, passing());
    let (out, result) = submit(
        &tool,
        json!({"tasks": [task("a", "a.py", &[]), task("b", "b.py", &[])], "verify_command": "pytest"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(status_of(&result, "a"), "integrated");
    assert_eq!(status_of(&result, "b"), "integrated");
    assert_eq!(
        (fx.read("a.py"), fx.read("b.py")),
        ("A\n".into(), "B\n".into())
    );
    assert_eq!(result["tasks"][0]["files_changed"], json!(["a.py"]));
    assert_eq!(result["tasks"][0]["report"], "a finished");
    assert_eq!(result["verification"]["passed"], true);
    assert!(!fx.worktrees_left());
}

#[tokio::test]
async fn a_dependent_task_starts_from_a_tree_holding_its_prerequisite() {
    let fx = Fixture::new();
    let repo = fx.repo.path().to_path_buf();
    let children = Children {
        edits: HashMap::from([
            ("a".into(), writes("a.py", "A\n")),
            (
                "b".into(),
                Box::new(|dir| {
                    let seen = std::fs::read_to_string(dir.join("a.py"))
                        .map_err(|_| "a.py missing".to_string())?;
                    std::fs::write(dir.join("b.py"), format!("B after {seen}"))
                        .map_err(|e| e.to_string())
                }),
            ),
        ]),
        ..Children::default()
    };
    let tool = fx.tool(children, passing());
    let (_, result) = submit(
        &tool,
        json!({"tasks": [task("b", "b.py", &["a"]), task("a", "a.py", &[])]}),
    )
    .await;
    assert_eq!(status_of(&result, "b"), "integrated", "{result}");
    assert_eq!(
        std::fs::read_to_string(repo.join("b.py")).unwrap(),
        "B after A\n"
    );
    assert!(result["next"]
        .as_str()
        .unwrap()
        .contains("No verification command"));
}

#[tokio::test]
async fn a_conflicting_pair_integrates_exactly_the_first_and_names_the_others_branch() {
    let fx = Fixture::new();
    let children = Children {
        // Both rewrite the line of a file neither owns; `b` finishes later.
        edits: HashMap::from([
            ("a".into(), writes("shared.txt", "ONE\ntwo\nthree\n")),
            ("b".into(), writes("shared.txt", "uno\ntwo\nthree\n")),
        ]),
        delays: HashMap::from([("b".into(), Duration::from_millis(1500))]),
        ..Children::default()
    };
    let tool = fx.tool(children, passing());
    let (_, result) = submit(
        &tool,
        json!({"tasks": [task("a", "a.py", &[]), task("b", "b.py", &[])]}),
    )
    .await;
    assert_eq!(status_of(&result, "a"), "integrated");
    assert_eq!(status_of(&result, "b"), "conflict");
    assert_eq!(fx.read("shared.txt"), "ONE\ntwo\nthree\n");
    let branch = result["tasks"][1]["branch"]
        .as_str()
        .expect("the kept branch is named");
    assert!(git(fx.repo.path(), &["branch", "--list", branch]).contains(branch));
    assert!(result["next"].as_str().unwrap().contains("Not everything"));
}

#[tokio::test]
async fn a_failed_child_skips_its_dependents_but_not_unrelated_tasks() {
    let fx = Fixture::new();
    let children = Children {
        edits: HashMap::from([
            (
                "a".into(),
                Box::new(|_: &Path| Err("could not build a".to_string())) as Edit,
            ),
            ("b".into(), writes("b.py", "B\n")),
            ("c".into(), writes("c.py", "C\n")),
        ]),
        ..Children::default()
    };
    let tool = fx.tool(children, passing());
    let (_, result) = submit(
        &tool,
        json!({"tasks": [task("a", "a.py", &[]), task("b", "b.py", &["a"]), task("c", "c.py", &[])]}),
    )
    .await;
    assert_eq!(status_of(&result, "a"), "failed");
    assert_eq!(status_of(&result, "b"), "skipped");
    assert_eq!(status_of(&result, "c"), "integrated");
    assert!(result["tasks"][0]["report"]
        .as_str()
        .unwrap()
        .contains("could not build a"));
    assert!(result["tasks"][1]["report"]
        .as_str()
        .unwrap()
        .contains("`a`"));
    assert!(result["tasks"][0]["branch"].is_string());
    assert_eq!(fx.read("b.py"), "");
}

#[tokio::test]
async fn a_failing_verification_returns_its_output_whatever_the_children_claim() {
    let fx = Fixture::new();
    let children = Children {
        edits: HashMap::from([("a".into(), writes("a.py", "A\n"))]),
        ..Children::default()
    };
    let tool = fx.tool(
        children,
        Verifier {
            passes: false,
            output: "FAILED test_a - assert 1 == 2",
        },
    );
    let (out, result) = submit(
        &tool,
        json!({"tasks": [task("a", "a.py", &[])], "verify_command": "pytest -x"}),
    )
    .await;
    assert!(!out.is_error);
    assert_eq!(status_of(&result, "a"), "integrated");
    assert_eq!(result["verification"]["passed"], false);
    assert_eq!(result["verification"]["command"], "pytest -x");
    assert!(result["verification"]["output"]
        .as_str()
        .unwrap()
        .contains("FAILED test_a"));
    assert!(result["next"].as_str().unwrap().contains("Repair"));
}

#[tokio::test]
async fn a_refused_graph_runs_nothing_and_costs_no_budget() {
    let fx = Fixture::new();
    // No edits scripted: a child that started would panic.
    let tool = fx.tool(Children::default(), passing());
    let cycle = json!({"tasks": [task("a", "a.py", &["b"]), task("b", "b.py", &["a"])]});
    for _ in 0..3 {
        let (out, _) = submit(&tool, cycle.clone()).await;
        assert!(
            out.is_error && out.content.contains("cycle"),
            "{}",
            out.content
        );
    }
    assert!(!fx.worktrees_left());
}

#[tokio::test]
async fn graphs_are_limited_per_session() {
    let fx = Fixture::new();
    let children = Children {
        edits: HashMap::from([
            ("a".into(), writes("a.py", "A\n")),
            ("b".into(), writes("b.py", "B\n")),
            ("c".into(), writes("c.py", "C\n")),
        ]),
        ..Children::default()
    };
    let tool = fx.tool(children, passing()); // limits(): max_graphs = 2
    for id in ["a", "b"] {
        let (out, _) = submit(
            &tool,
            json!({"tasks": [task(id, &format!("{id}.py"), &[])]}),
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
    }
    let (out, _) = submit(&tool, json!({"tasks": [task("c", "c.py", &[])]})).await;
    assert!(
        out.is_error && out.content.contains("already run 2 task graphs"),
        "{}",
        out.content
    );
    assert_eq!(fx.read("c.py"), "");
}

#[test]
fn plan_tasks_is_offered_only_when_orchestration_is_plan() {
    let scope = sven_tool_api::PathScope::unconfined();
    let offered = |orchestration| {
        let mut config = Config::default();
        config.agent.orchestration = orchestration;
        let mut registry = sven_tool_registry::ToolRegistry::new();
        register(&mut registry, Arc::new(make_task()), &config, &scope);
        registry.get("plan_tasks").is_some()
    };
    assert!(!offered(Orchestration::Off));
    assert!(offered(Orchestration::Plan));
}
