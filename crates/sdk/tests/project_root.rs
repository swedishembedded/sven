// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a project root makes one directory the agent's world.
//!
//! An embedding application says "this agent works here", and the built-in
//! file tools resolve against that directory and refuse to leave it, the
//! shell starts in it, and the run's own records land in it - wherever the
//! process happens to be running.
//!
//! This file runs as its own test process: every test moves the process into
//! the same scratch directory, standing in for an application whose working
//! directory has nothing to do with the agent's, and the tests are serialised
//! so none observes another's run.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in confining what an agent can touch then you
//! can procure our services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, Toolset};

/// The process working directory every test in this file runs from: a fresh
/// directory the agent has no business writing to.
fn process_cwd() -> &'static Path {
    static CWD: OnceLock<tempfile::TempDir> = OnceLock::new();
    CWD.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a scratch cwd");
        std::env::set_current_dir(dir.path()).expect("enter the scratch cwd");
        dir
    })
    .path()
}

/// Serialises the tests in this file, having moved the process into
/// [`process_cwd`].
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    process_cwd();
    guard
}

/// A project root with a sibling directory outside it.
struct Workspace {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

fn workspace() -> Workspace {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let base = tmp.path().canonicalize().expect("a real path");
    let root = base.join("project");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret").unwrap();
    Workspace {
        _tmp: tmp,
        root,
        outside,
    }
}

/// Runs one agent turn on a coding engine rooted at `root` in which the
/// model makes each of `calls` in turn, and returns each call's result text
/// and whether it was an error, in order.
async fn run_calls(root: &Path, calls: &[(&str, serde_json::Value)]) -> Vec<String> {
    let mut scripts: Vec<Vec<ResponseEvent>> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: format!("c{i}"),
                    name: (*name).into(),
                    arguments: args.to_string(),
                },
                ResponseEvent::Done,
            ]
        })
        .collect();
    scripts.push(vec![
        ResponseEvent::TextDelta("done".into()),
        ResponseEvent::Done,
    ]);
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .toolset(Toolset::coding())
        .project_root(root)
        .build()
        .expect("an engine with an existing root builds");
    let mut agent = engine.agent("agent");
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), agent.send("work"))
        .await
        .expect("the run finishes")
        .expect("an outcome");
    assert_eq!(outcome.reply, "done");

    agent
        .suspend()
        .history()
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::ToolResult { content, .. } => Some(content.to_string()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_file_tools_resolve_inside_the_root_and_refuse_to_leave_it() {
    let _serial = exclusive().await;
    let ws = workspace();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&ws.outside, ws.root.join("escape")).unwrap();

    let outside_abs = ws.outside.join("secret.txt");
    let results = run_calls(
        &ws.root,
        &[
            (
                "write_file",
                serde_json::json!({"path": "notes/hello.txt", "text": "hi", "append": false}),
            ),
            (
                "read_file",
                serde_json::json!({"path": "../outside/secret.txt"}),
            ),
            (
                "read_file",
                serde_json::json!({"path": outside_abs.to_string_lossy()}),
            ),
            (
                "read_file",
                serde_json::json!({"path": "escape/secret.txt"}),
            ),
            (
                "write_file",
                serde_json::json!({"path": "escape/planted.txt", "text": "x", "append": false}),
            ),
            (
                "grep",
                serde_json::json!({"pattern": "secret", "path": "../outside"}),
            ),
            (
                "read_file",
                serde_json::json!({"path": ws.root.join("notes/hello.txt").to_string_lossy()}),
            ),
        ],
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(ws.root.join("notes/hello.txt")).unwrap(),
        "hi",
        "a relative write lands inside the root: {results:?}"
    );
    assert!(
        !process_cwd().join("notes").exists(),
        "nothing is written relative to the process cwd"
    );

    let root = ws.root.display().to_string();
    for refused in &results[1..6] {
        assert!(
            refused.contains("outside the project root") && refused.contains(&root),
            "a path outside the root is refused, naming the root: {refused}"
        );
        assert!(!refused.contains("the secret"), "{refused}");
    }
    assert!(
        !ws.outside.join("planted.txt").exists(),
        "a write through a symlink out of the root is refused"
    );
    assert!(
        results[6].contains("hi"),
        "an absolute path inside the root is fine: {}",
        results[6]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_shell_starts_in_the_root_and_refuses_a_workdir_outside_it() {
    let _serial = exclusive().await;
    let ws = workspace();
    let results = run_calls(
        &ws.root,
        &[
            (
                "shell",
                serde_json::json!({"description": "where am I", "shell_command": "pwd -P"}),
            ),
            (
                "shell",
                serde_json::json!({
                    "description": "look outside",
                    "shell_command": "ls",
                    "workdir": ws.outside.to_string_lossy(),
                }),
            ),
        ],
    )
    .await;

    assert_eq!(results[0].trim(), ws.root.display().to_string());
    assert!(
        results[1].contains("outside the project root"),
        "{}",
        results[1]
    );
    assert!(!results[1].contains("secret.txt"), "{}", results[1]);
}

#[tokio::test]
async fn the_audit_log_is_kept_in_the_root_not_the_process_cwd() {
    let _serial = exclusive().await;
    let ws = workspace();
    run_calls(
        &ws.root,
        &[("read_file", serde_json::json!({"path": "missing.txt"}))],
    )
    .await;

    let log = ws.root.join(".sven").join("audit.jsonl");
    assert!(
        std::fs::metadata(&log).is_ok_and(|m| m.len() > 0),
        "the audit log is written to {}",
        log.display()
    );
    assert!(
        !process_cwd().join(".sven").exists(),
        "the run writes nothing to the process cwd"
    );
}

#[test]
fn a_root_that_is_not_a_directory_is_refused_when_the_engine_is_built() {
    let ws = workspace();
    let file = ws.root.join("file");
    std::fs::write(&file, "x").unwrap();
    for bad in [ws.root.join("missing"), file] {
        let err = Engine::builder()
            .project_root(&bad)
            .build()
            .err()
            .unwrap_or_else(|| panic!("{} was accepted as a root", bad.display()));
        assert!(
            err.to_string().contains(&bad.display().to_string()),
            "{err}"
        );
    }
}
