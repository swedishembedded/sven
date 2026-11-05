// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: with a race-free scope (a served run's), the file tools resolve and
//! open every path beneath the workspace inside the kernel, so a process that
//! swaps a workspace directory for a symlink to the outside, again and again
//! while the tools run, never makes a tool read or write outside the
//! workspace. The old resolver (resolve, then open by path) is shown beside
//! it: it checks a path and then uses it, and the swap lands in between.
//!
//! Own test binary: it runs a thread that rewrites the tree concurrently.
//!
//! Swedish Embedded AB implements solutions for confining agent file access
//! without time-of-check gaps for its clients. If your team needs expertise
//! in sandboxing untrusted automation then you can procure our services by
//! sending an email to info@swedishembedded.com.
#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sven_tool_api::{PathScope, Tool, ToolCall, ToolOutput};
use sven_tools_fs::{EditFileTool, ReadFileTool, WriteTool};

const ITERATIONS: usize = 2000;
const MARKER: &str = "OUTSIDE-MARKER";

/// `workspace/sub` is a real directory holding `secret` and `edit.txt`;
/// `outside` holds the same names with the marker. `swap` is a symlink to
/// `outside` that [`Flipper`] exchanges with `sub` atomically.
struct Tree {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
}

fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(workspace.join("sub/secret"), "inside\n").unwrap();
    std::fs::write(workspace.join("sub/edit.txt"), "alpha\n").unwrap();
    std::fs::write(outside.join("secret"), format!("{MARKER}\n")).unwrap();
    std::fs::write(outside.join("edit.txt"), "alpha\n").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("swap")).unwrap();
    Tree {
        _tmp: tmp,
        workspace,
        outside,
    }
}

/// Exchanges `sub` and `swap` (a directory and a symlink) in one atomic
/// `renameat2(RENAME_EXCHANGE)`, as fast as it can, until dropped.
struct Flipper {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Flipper {
    fn start(workspace: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (a, b) = (
            CString::new(workspace.join("sub").to_str().unwrap()).unwrap(),
            CString::new(workspace.join("swap").to_str().unwrap()).unwrap(),
        );
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                // SAFETY: two NUL-terminated absolute paths.
                unsafe {
                    libc::syscall(
                        libc::SYS_renameat2,
                        libc::AT_FDCWD,
                        a.as_ptr(),
                        libc::AT_FDCWD,
                        b.as_ptr(),
                        libc::RENAME_EXCHANGE,
                    );
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Flipper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn race_free(workspace: &Path) -> PathScope {
    PathScope::confined(workspace).unwrap().race_free().unwrap()
}

async fn run(tool: &dyn Tool, args: Value) -> ToolOutput {
    tool.execute(&ToolCall {
        id: "c".into(),
        name: tool.name().into(),
        args,
    })
    .await
}

#[tokio::test]
async fn a_symlink_to_the_outside_is_refused_by_every_scope() {
    let t = tree();
    std::fs::remove_file(t.workspace.join("swap")).unwrap();
    std::os::unix::fs::symlink(t.outside.join("secret"), t.workspace.join("link")).unwrap();
    for scope in [
        PathScope::confined(&t.workspace).unwrap(),
        race_free(&t.workspace),
    ] {
        let out = run(&ReadFileTool::new(scope), json!({ "path": "link" })).await;
        assert!(out.is_error, "{}", out.content);
        assert!(!out.content.contains(MARKER), "{}", out.content);
    }
}

#[tokio::test]
async fn a_file_inside_the_workspace_is_read_through_an_inside_link() {
    let t = tree();
    std::os::unix::fs::symlink("sub/secret", t.workspace.join("alias")).unwrap();
    let out = run(
        &ReadFileTool::new(race_free(&t.workspace)),
        json!({ "path": "alias" }),
    )
    .await;
    assert!(
        !out.is_error && out.content.contains("inside"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn read_file_never_returns_a_file_from_outside_while_a_directory_is_swapped() {
    let t = tree();
    let flipper = Flipper::start(&t.workspace);

    // The old resolver: check the path, then use it by name. Reported, not
    // asserted: whether the swap lands in the window depends on scheduling.
    let old = PathScope::confined(&t.workspace).unwrap();
    let old_leaks = (0..ITERATIONS)
        .filter(|_| {
            old.resolve("sub/secret")
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .is_some_and(|text| text.contains(MARKER))
        })
        .count();
    eprintln!("old resolver leaked {old_leaks} of {ITERATIONS}");

    let tool = ReadFileTool::new(race_free(&t.workspace));
    let (mut read, mut leaked) = (0, 0);
    for _ in 0..ITERATIONS {
        let out = run(&tool, json!({ "path": "sub/secret" })).await;
        if out.content.contains(MARKER) {
            leaked += 1;
        } else if !out.is_error {
            read += 1;
        }
    }
    drop(flipper);
    eprintln!("race-free read_file: {read} reads, {leaked} leaks of {ITERATIONS}");
    assert_eq!(
        leaked, 0,
        "read_file returned a file from outside the workspace"
    );
}

#[tokio::test]
async fn write_file_never_lands_outside_while_a_directory_is_swapped() {
    let t = tree();
    let flipper = Flipper::start(&t.workspace);
    let tool = WriteTool::new(race_free(&t.workspace));
    for i in 0..ITERATIONS {
        // Both a file in the swapped directory and a file in a directory
        // that has to be created below it.
        run(
            &tool,
            json!({ "path": "sub/new.txt", "text": format!("w{i}") }),
        )
        .await;
        run(
            &tool,
            json!({ "path": "sub/deep/new.txt", "text": "w", "append": true }),
        )
        .await;
    }
    drop(flipper);
    for name in ["new.txt", "deep"] {
        assert!(
            !t.outside.join(name).exists(),
            "{name} was created outside the workspace"
        );
    }
}

#[tokio::test]
async fn edit_file_never_changes_a_file_outside_while_a_directory_is_swapped() {
    let t = tree();
    let flipper = Flipper::start(&t.workspace);
    let tool = EditFileTool::new(race_free(&t.workspace));
    for _ in 0..ITERATIONS {
        let out = run(
            &tool,
            json!({ "path": "sub/edit.txt", "diff": "@@\n-alpha\n+beta\n" }),
        )
        .await;
        if !out.is_error {
            // Put it back so the next edit has something to change.
            run(
                &tool,
                json!({ "path": "sub/edit.txt", "diff": "@@\n-beta\n+alpha\n" }),
            )
            .await;
        }
    }
    drop(flipper);
    assert_eq!(
        std::fs::read_to_string(t.outside.join("edit.txt")).unwrap(),
        "alpha\n",
        "edit_file changed a file outside the workspace"
    );
}
