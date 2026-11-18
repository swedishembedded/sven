// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a `ShellTool` given a launcher starts `launcher args... bash -c
//! <command>` instead of `bash -c <command>`, in the call's working
//! directory, and the call's output, exit status and rendering are those of
//! the command; a launcher together with a confinement is refused.
//!
//! Swedish Embedded AB implements solutions for running agent commands in an
//! application's own sandbox for its clients. If your team needs expertise in
//! sandboxing and isolating untrusted command execution then you can procure
//! our services by sending an email to info@swedishembedded.com.
#![cfg(target_os = "linux")]

use std::path::Path;

use serde_json::json;
use sven_tool_api::{PathScope, Tool, ToolCall, ToolOutput};
use sven_tools_exec::confine::CommandConfinement;
use sven_tools_exec::ShellTool;

/// A launcher that records its argv (one per line) and its cwd, then runs the
/// rest of its arguments.
fn recording_launcher(dir: &Path) -> Vec<String> {
    let script = dir.join("launch.sh");
    let record = dir.join("record");
    sven_test_support::write_executable(
        &script,
        &format!(
            "#!/bin/sh\n{{ pwd; for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > '{}'\nshift\nexec \"$@\"\n",
            record.display()
        ),
    );
    vec![script.display().to_string(), "--flag".into()]
}

async fn run(shell: &ShellTool, command: &str) -> ToolOutput {
    shell
        .execute(&ToolCall {
            id: "c".into(),
            name: "shell".into(),
            args: json!({ "shell_command": command }),
        })
        .await
}

/// Output with the run-dependent duration removed.
fn stable(out: &ToolOutput) -> String {
    out.content
        .lines()
        .map(|l| l.split(" | Duration").next().unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_command_runs_through_the_launcher_in_the_calls_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let launcher = recording_launcher(tmp.path());
    let command = "echo out; echo err >&2; exit 3";

    let plain = in_tree(work.path());
    let launched = ShellTool {
        launcher: Some(launcher),
        ..in_tree(work.path())
    };
    let expected = run(&plain, command).await;
    let got = run(&launched, command).await;

    assert_eq!(stable(&got), stable(&expected), "{}", got.content);
    assert!(got.is_error && expected.is_error);
    assert!(got.content.contains("Exit: 3"), "{}", got.content);

    let record = std::fs::read_to_string(tmp.path().join("record")).unwrap();
    let lines: Vec<&str> = record.lines().collect();
    let cwd = std::fs::canonicalize(work.path()).unwrap();
    assert_eq!(Path::new(lines[0]), cwd);
    assert_eq!(&lines[1..], ["--flag", "bash", "-c", command]);
}

fn in_tree(work: &Path) -> ShellTool {
    ShellTool {
        scope: PathScope::confined(work).unwrap(),
        ..ShellTool::default()
    }
}

#[tokio::test]
async fn a_launcher_together_with_a_confinement_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = ShellTool {
        launcher: Some(recording_launcher(tmp.path())),
        confine: Some(CommandConfinement {
            writable: vec![tmp.path().to_path_buf()],
            ..CommandConfinement::default()
        }),
        ..ShellTool::default()
    };
    let out = run(&shell, "echo hi").await;
    assert!(out.is_error);
    assert!(out.content.contains("launcher"), "{}", out.content);
    assert!(!tmp.path().join("record").exists(), "nothing ran");
}
