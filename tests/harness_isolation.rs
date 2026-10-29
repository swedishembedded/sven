// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: many harness runs share one engine, one model and one process and
//! stay apart. With the real coding tools, each run's files, shell and gates
//! live in the workspace it was given and nowhere else; a run that cheats
//! fails its own gate, and only its own run fails;
//! cancelling one run leaves the rest to finish; and the coding profile
//! offers the tools, with the argument names, the prompts of a port use.
//!
//! The model is a function of the request (`CallbackProvider`): each run's
//! solver reads its identity from the system message the workflow renders
//! from the run's inputs, and the stage it is in from the tool results so far,
//! so one shared model serves every run without any per-run state.
//!
//! Swedish Embedded AB implements verifiable agent test benches for its
//! clients. If your team needs expertise in auditing agent pipelines with
//! scripted adversaries then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::{CompletionRequest, MessageContent, ResponseEvent, Role};
use sven_model_mock::CallbackProvider;
use sven_sdk::{CancelToken, Engine, Harness, HarnessOutcome, RunConclusion, RunOptions, Toolset};

/// A workflow whose agent writes its identity into `owner.txt` and records
/// where its shell runs; the gate reads the file back.
const WORKFLOW: &str = r#"
api_version: "2"
name: tree-writer
inputs: { who: string }
agent:
  profile: coding
  tools: [write_file, shell, read_file]
  system: "You are agent {{ inputs.who }}."
execution: { limits: { model_turns: 8, tool_calls: 8 } }
gates:
  owner: { command: [grep, -qx, "{{ inputs.who }}", owner.txt], timeout: 20 }
workflow:
  - name: work
    task: Write your identity into owner.txt, then say done.
    verify: [owner]
outputs:
  summary: { type: string, value: "{{ tasks.work.output }}" }
"#;

fn call(index: usize, name: &str, arguments: &Value) -> ResponseEvent {
    ResponseEvent::ToolCall {
        index: 0,
        id: format!("call-{index}"),
        name: name.to_string(),
        arguments: arguments.to_string(),
    }
}

/// The text of the request's system message.
fn system_text(req: &CompletionRequest) -> String {
    req.messages
        .iter()
        .find(|m| m.role == Role::System)
        .and_then(|m| match &m.content {
            MessageContent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// One shared solver for every run. Who it is comes from the system
/// message; what it does next from how many tool results it has seen. A
/// `cheat` agent writes someone else's name; a `slow` one never stops
/// working.
fn solver(req: &CompletionRequest) -> Vec<ResponseEvent> {
    let system = system_text(req);
    let who = system
        .trim_start_matches("You are agent ")
        .trim_end_matches('.')
        .to_string();
    let seen = req.messages.iter().filter(|m| m.role == Role::Tool).count();
    let events = match (seen, who.as_str()) {
        (_, "slow") => vec![call(seen, "shell", &json!({"shell_command": "sleep 30"}))],
        (0, "cheat") => vec![call(
            0,
            "write_file",
            &json!({"path": "owner.txt", "text": "somebody-else\n"}),
        )],
        (0, _) => vec![call(
            0,
            "write_file",
            &json!({"path": "owner.txt", "text": format!("{who}\n")}),
        )],
        (1, _) => vec![call(
            1,
            "shell",
            &json!({"shell_command": "pwd > where.txt"}),
        )],
        _ => vec![ResponseEvent::TextDelta(format!("{who} done"))],
    };
    events
        .into_iter()
        .chain(std::iter::once(ResponseEvent::Done))
        .collect()
}

fn engine() -> Engine {
    Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(solver)) as Arc<_>)
        .toolset(Toolset::coding())
        .build()
        .expect("an engine builds")
}

async fn run_in(
    harness: &Harness,
    who: &str,
    dir: &Path,
    cancel: Option<CancelToken>,
) -> HarnessOutcome {
    let mut run = harness
        .start(json!({"who": who}))
        .expect("starts")
        .in_workspace(dir)
        .expect("a directory");
    let mut options = RunOptions::new();
    if let Some(token) = cancel {
        options = options.cancel(token);
    }
    tokio::time::timeout(Duration::from_secs(60), run.execute(options))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_runs_on_one_engine_each_work_only_in_their_own_tree() {
    let engine = engine();
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let names: Vec<String> = (0..8).map(|n| format!("agent{n}")).collect();
    let trees: Vec<tempfile::TempDir> = names
        .iter()
        .map(|_| tempfile::tempdir().expect("a tempdir"))
        .collect();

    let mut running = tokio::task::JoinSet::new();
    for (who, tree) in names.iter().zip(&trees) {
        let (harness, who, dir) = (harness.clone(), who.clone(), tree.path().to_path_buf());
        running.spawn(async move { (who.clone(), run_in(&harness, &who, &dir, None).await) });
    }
    let mut outcomes = std::collections::BTreeMap::new();
    while let Some(done) = running.join_next().await {
        let (who, outcome) = done.expect("the run task ends");
        outcomes.insert(who, outcome);
    }

    for (who, tree) in names.iter().zip(&trees) {
        let outcome = &outcomes[who];
        assert!(
            matches!(&outcome.result, Some(Ok(_))),
            "{who}: {:?}",
            outcome.result
        );
        assert_eq!(read(tree.path(), "owner.txt").trim(), who, "{who}'s file");
        let shell_cwd = read(tree.path(), "where.txt");
        assert_eq!(
            Path::new(shell_cwd.trim()).canonicalize().unwrap(),
            tree.path().canonicalize().unwrap(),
            "{who}'s shell ran in its own tree"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cheating_run_fails_its_own_gate_and_the_others_complete() {
    let engine = engine();
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let (honest_tree, cheat_tree) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (honest, cheat) = tokio::join!(
        run_in(&harness, "honest", honest_tree.path(), None),
        run_in(&harness, "cheat", cheat_tree.path(), None)
    );
    assert!(matches!(&honest.result, Some(Ok(_))), "{:?}", honest.result);
    let Some(Err(failure)) = &cheat.result else {
        panic!("the cheat's gate fails: {:?}", cheat.result);
    };
    assert_eq!(failure.code, "gate_failed");
    // A failed run is not a success (crates/sdk/tests/harness_run.rs); what
    // isolation adds is that only the cheat's run failed.
    assert_eq!(cheat.conclusion, RunConclusion::AgentError);
    assert_eq!(honest.conclusion, RunConclusion::Success);
    assert_eq!(read(honest_tree.path(), "owner.txt").trim(), "honest");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_one_run_leaves_the_others_to_finish() {
    let engine = engine();
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let (slow_tree, fast_tree) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let token = CancelToken::new();
    let stopper = {
        let token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            token.cancel();
        })
    };
    let (slow, fast) = tokio::join!(
        run_in(&harness, "slow", slow_tree.path(), Some(token)),
        run_in(&harness, "fast", fast_tree.path(), None)
    );
    stopper.await.unwrap();
    assert_eq!(slow.conclusion, RunConclusion::Cancelled);
    assert!(matches!(&fast.result, Some(Ok(_))), "{:?}", fast.result);
}

/// The tools and argument names a port's prompts rely on.
#[tokio::test]
async fn the_coding_profile_offers_the_tools_a_port_names() {
    let seen = Arc::new(std::sync::Mutex::new(Value::Null));
    let record = Arc::clone(&seen);
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |req| {
            *record.lock().unwrap() = serde_json::to_value(&req.tools).unwrap();
            vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
        })) as Arc<_>)
        .toolset(Toolset::coding())
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(
            r#"
api_version: "2"
name: offer
agent: { profile: coding }
execution: { limits: { model_turns: 2 } }
workflow:
  - { name: look, task: "Look around." }
"#,
        )
        .expect("loads");
    let tree = tempfile::tempdir().unwrap();
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(tree.path())
        .unwrap();
    run.execute(RunOptions::new()).await.expect("an outcome");

    let tools = seen.lock().unwrap().clone();
    let properties = |name: &str| -> Vec<String> {
        let tool = tools
            .as_array()
            .and_then(|all| {
                all.iter()
                    .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
            })
            .unwrap_or_else(|| panic!("the coding profile offers `{name}`: {tools}"));
        tool.pointer("/parameters/properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default()
    };
    for (tool, argument) in [
        ("read_file", "path"),
        ("write_file", "path"),
        ("write_file", "text"),
        ("edit_file", "path"),
        ("find_file", "pattern"),
        ("grep", "pattern"),
        ("grep", "path"),
        ("grep", "include"),
        ("grep", "context_lines"),
        ("shell", "shell_command"),
        ("todo", "todos"),
    ] {
        assert!(
            properties(tool).iter().any(|p| p == argument),
            "`{tool}` takes `{argument}`: {:?}",
            properties(tool)
        );
    }
}
