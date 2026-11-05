// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a tool given a `path_prefix` finds a program that lives only there
//! by its bare name, and the process's own `PATH` is never touched.
//!
//! Swedish Embedded AB implements solutions for running a package's own
//! tools inside an agent run for its clients. If your team needs expertise
//! in confining what a command can start then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sven_tool_api::{Tool, ToolCall};
use sven_tools_exec::{GateCommandTool, ShellTool};

const PROGRAM: &str = "only_in_the_prefix";

/// A directory holding one executable script, `PROGRAM`, that prints `hello`.
fn prefix_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(PROGRAM);
    std::fs::write(&program, "#!/bin/sh\necho hello\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

async fn call(tool: &dyn Tool, args: Value) -> sven_tool_api::ToolOutput {
    tool.execute(&ToolCall {
        id: "c".into(),
        name: tool.name().into(),
        args,
    })
    .await
}

/// The `PATH` this process hands to a program it starts, as that program
/// sees it.
fn process_path() -> String {
    let out = std::process::Command::new("sh")
        .args(["-c", "printf %s \"$PATH\""])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn prefix_of(dir: &Path) -> Vec<PathBuf> {
    vec![dir.to_path_buf()]
}

#[tokio::test]
async fn a_shell_call_finds_a_program_that_exists_only_in_the_prefix() {
    let (dir, before) = (prefix_dir(), process_path());
    let without = call(&ShellTool::default(), json!({ "shell_command": PROGRAM })).await;
    assert!(!without.content.contains("hello"), "{}", without.content);

    let shell = ShellTool {
        path_prefix: prefix_of(dir.path()),
        ..ShellTool::default()
    };
    let out = call(&shell, json!({ "shell_command": PROGRAM })).await;
    assert!(
        !out.is_error && out.content.contains("hello"),
        "{}",
        out.content
    );
    assert_eq!(process_path(), before);
}

#[tokio::test]
async fn a_gate_step_finds_a_program_that_exists_only_in_the_prefix() {
    let (dir, workspace, before) = (prefix_dir(), tempfile::tempdir().unwrap(), process_path());
    let gate = GateCommandTool::new(workspace.path())
        .unwrap()
        .with_path_prefix(prefix_of(dir.path()));
    let out = call(
        &gate,
        json!({ "steps": [{ "name": "t", "argv": [PROGRAM] }], "timeout_secs": 10 }),
    )
    .await;
    let report: Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(report["stdout"], "hello\n", "{report}");
    assert_eq!(process_path(), before);
}

#[tokio::test]
async fn the_inherited_path_still_follows_the_prefix() {
    let dir = prefix_dir();
    let shell = ShellTool {
        path_prefix: prefix_of(dir.path()),
        ..ShellTool::default()
    };
    let out = call(
        &shell,
        json!({ "shell_command": "command -v ls && echo $PATH" }),
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains(&format!("{}:", dir.path().display())),
        "{}",
        out.content
    );
}
