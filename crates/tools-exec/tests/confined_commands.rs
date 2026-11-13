// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a `ShellTool` given a `CommandConfinement` starts every command
//! inside a Linux Landlock domain that reaches only the workspace and what a
//! program needs to run, and no network; a gate command is not confined.
//!
//! Own test binary: it starts processes and reads files outside the temp
//! directories, so it shares no state with the unit tests.
//!
//! Swedish Embedded AB implements solutions for running untrusted agent
//! commands next to trusted workers for its clients. If your team needs
//! expertise in confining what an agent can touch then you can procure our
//! services by sending an email to info@swedishembedded.com.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{json, Value};
use sven_tool_api::{Tool, ToolCall, ToolOutput};
use sven_tools_exec::confine::CommandConfinement;
use sven_tools_exec::{GateCommandTool, ShellTool};

/// Directories of a run: its workspace, the package's `bin` (a script and the
/// library it sources), and a worker-private directory holding a stand-in for
/// the run's token and for a gate script.
struct Layout {
    _root: tempfile::TempDir,
    workspace: std::path::PathBuf,
    bin: std::path::PathBuf,
    private: std::path::PathBuf,
}

fn layout() -> Layout {
    let root = tempfile::tempdir().unwrap();
    let (workspace, bin, private) = (
        root.path().join("workspace"),
        root.path().join("package/bin"),
        root.path().join("worker"),
    );
    for dir in [&workspace, &bin, &private] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(bin.join("lib.sh"), "greeting=hello-from-lib\n").unwrap();
    let tool = bin.join("package-tool");
    std::fs::write(&tool, ". \"${0%/*}/lib.sh\"\necho \"$greeting\"\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(private.join("token"), "secret-token").unwrap();
    let gate_script = private.join("gate.sh");
    std::fs::write(&gate_script, "#!/bin/sh\necho gate-ran\ncat \"$1\"\n").unwrap();
    std::fs::set_permissions(&gate_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Layout {
        _root: root,
        workspace,
        bin,
        private,
    }
}

fn confinement(layout: &Layout) -> CommandConfinement {
    CommandConfinement {
        writable: vec![layout.workspace.clone()],
        read_exec: vec![layout.bin.clone()],
        tmpdir: Some(layout.workspace.join(".tmp")),
        connect_ports: Vec::new(),
    }
}

fn shell(layout: &Layout) -> ShellTool {
    std::fs::create_dir_all(layout.workspace.join(".tmp")).unwrap();
    ShellTool {
        path_prefix: vec![layout.bin.clone()],
        confine: Some(confinement(layout)),
        ..ShellTool::default()
    }
}

async fn run(tool: &dyn Tool, args: Value) -> ToolOutput {
    tool.execute(&ToolCall {
        id: "c".into(),
        name: tool.name().into(),
        args,
    })
    .await
}

async fn sh(tool: &ShellTool, workdir: &Path, command: &str) -> ToolOutput {
    run(
        tool,
        json!({ "shell_command": command, "workdir": workdir.display().to_string() }),
    )
    .await
}

#[tokio::test]
async fn a_confined_command_works_in_the_workspace_and_runs_the_packages_tools() {
    let l = layout();
    let tool = shell(&l);
    let out = sh(
        &tool,
        &l.workspace,
        "echo made > made.txt && read -r m < made.txt && echo \"$m\" && package-tool \
         && echo \"$TMPDIR\" && echo x > /dev/null && [ -d /usr/bin ]",
    )
    .await;
    assert!(
        !out.is_error && out.content.contains("made") && out.content.contains("hello-from-lib"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains(".tmp"),
        "TMPDIR is the private tmp: {}",
        out.content
    );
    assert!(l.workspace.join("made.txt").is_file());
}

#[tokio::test]
async fn a_confined_command_reaches_nothing_outside_the_workspace() {
    let l = layout();
    let tool = shell(&l);
    let token = l.private.join("token");
    let gate = l.private.join("gate.sh");
    let outside = l.private.join("planted");
    let shared_tmp =
        std::env::temp_dir().join(format!("sven-confine-escape-{}", std::process::id()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    for (what, command) in [
        (
            "read the token file",
            format!("read -r l < {}", token.display()),
        ),
        (
            "read a gate script",
            format!("read -r l < {}", gate.display()),
        ),
        (
            "list the worker directory",
            format!("exec 3< {}", l.private.display()),
        ),
        ("write outside", format!("echo x > {}", outside.display())),
        (
            "write the shared tmp",
            format!("echo x > {}", shared_tmp.display()),
        ),
        (
            "connect over tcp",
            format!("exec 3<>/dev/tcp/127.0.0.1/{port}"),
        ),
        (
            "bind a port",
            "perl -e 'use Socket; socket(S, PF_INET, SOCK_STREAM, 0); \
             bind(S, sockaddr_in(0, inet_aton(\"127.0.0.1\"))) or die \"bind: $!\"'"
                .into(),
        ),
    ] {
        let out = sh(&tool, &l.workspace, &command).await;
        assert!(
            out.content.contains("Permission denied") && !out.content.contains("Exit: 0"),
            "the command could {what}: {}",
            out.content
        );
    }
    assert!(!outside.exists());
    assert!(!shared_tmp.exists());
    // The listener saw no connection.
    listener.set_nonblocking(true).unwrap();
    assert!(listener.accept().is_err(), "a confined command connected");
}

#[tokio::test]
async fn a_gate_command_in_the_same_session_is_not_confined() {
    let l = layout();
    let _tool = shell(&l);
    let gate = GateCommandTool::new(&l.workspace).unwrap();
    let script = l.private.join("gate.sh");
    let token = l.private.join("token");
    let out = run(
        &gate,
        json!({
            "steps": [{
                "name": "g",
                "argv": [script.display().to_string(), token.display().to_string()]
            }],
            "timeout_secs": 10
        }),
    )
    .await;
    let report: Value = serde_json::from_str(&out.content).unwrap();
    assert_eq!(report["stdout"], "gate-ran\nsecret-token", "{report}");
}

#[test]
fn a_kernel_without_the_full_rule_set_refuses_to_start_a_confined_run() {
    use sven_tools_exec::confine::require_abi;
    assert!(require_abi(6).is_ok());
    assert!(require_abi(7).is_ok());
    for old in [5, 4, 1, 0, -1] {
        let error = require_abi(old).expect_err("too old a kernel");
        assert!(error.contains("ABI 6"), "{error}");
    }
}

/// Documented limit, not a guarantee: a command that leaves its process
/// group (`setsid`) outlives its call, because the shell tool kills the
/// group. It stays inside its Landlock domain, so what it can reach does not
/// grow; the file tools' race-free paths (not this) are what make it
/// harmless to the worker.
#[tokio::test]
async fn a_command_that_starts_its_own_session_outlives_its_call_still_confined() {
    let l = layout();
    let tool = shell(&l);
    let out = sh(
        &tool,
        &l.workspace,
        "setsid sh -c 'echo $$ > bg.pid; perl -e \"select(undef,undef,undef,3)\"; \
         read -r l < ../worker/token > bg.out 2>&1; echo $l >> bg.out' \
         > /dev/null 2>&1 < /dev/null & perl -e 'select(undef,undef,undef,0.5)'",
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    let pid = std::fs::read_to_string(l.workspace.join("bg.pid"))
        .expect("the background command started")
        .trim()
        .to_string();
    assert!(
        Path::new(&format!("/proc/{pid}")).exists(),
        "it survived the call"
    );
    // Still confined: its read of the private token is refused.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let attempt = std::fs::read_to_string(l.workspace.join("bg.out")).unwrap();
    assert!(!attempt.contains("secret-token"), "{attempt}");
}

/// A process of the host, outside the confinement, that a confined command
/// must not learn anything about.
struct Bystander(std::process::Child);

impl Bystander {
    fn start() -> Self {
        Self(
            std::process::Command::new("sleep")
                .arg("120")
                .env("BYSTANDER_SECRET", "host-environment-value")
                .spawn()
                .unwrap(),
        )
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reads `path` with the shell's own redirection, so the answer is the
/// kernel's and no host program is involved.
fn read_in_shell(path: &str) -> String {
    format!(": < {path} && echo READ-OK")
}

/// The threat model: a tool spawned for a (possibly prompt-poisoned) model
/// reaches only the workspace tree. Nothing of `/proc/<pid>` is reachable,
/// not another process's entries (command line, environment, auxv, maps,
/// status), not the host's, not the confined command's own, whatever
/// process the command is, a grandchild of the shell included.
#[tokio::test]
async fn a_confined_command_reads_no_proc_entry_of_any_process() {
    let l = layout();
    let tool = shell(&l);
    let bystander = Bystander::start();
    let mut targets = vec![
        format!("/proc/{}", bystander.pid()),
        format!("/proc/{}", std::process::id()),
        "/proc/self".to_string(),
        "/proc/thread-self".to_string(),
    ];
    targets.dedup();
    for base in &targets {
        for entry in ["cmdline", "environ", "auxv", "status", "maps", "stat"] {
            let command = read_in_shell(&format!("{base}/{entry}"));
            let out = sh(&tool, &l.workspace, &command).await;
            assert!(
                out.content.contains("Permission denied") && !out.content.contains("READ-OK"),
                "a confined command read {base}/{entry}: {}",
                out.content
            );
            // The same from a grandchild of the shell, with a pid of its own.
            let nested = format!("sh -c '{command}'");
            let out = sh(&tool, &l.workspace, &nested).await;
            assert!(
                out.content.contains("Permission denied") && !out.content.contains("READ-OK"),
                "a grandchild read {base}/{entry}: {}",
                out.content
            );
        }
    }
    // The machine-wide files the profile does allow stay readable.
    let out = sh(&tool, &l.workspace, &read_in_shell("/proc/meminfo")).await;
    assert!(out.content.contains("READ-OK"), "{}", out.content);
}

/// Why the profile has no "own auxv" exception. A Landlock rule names an
/// inode, and `/proc/self` is resolved when the rule is made: a rule made by
/// the worker names the worker's process, never the command's, and no rule
/// made ahead of time can name the pid of a command (or its grandchildren)
/// that does not exist yet. The only rule that would serve them all is
/// `/proc` itself, and that opens every other process's command line to the
/// tool. Both halves are shown here so a change of mind starts from facts.
#[tokio::test]
async fn no_landlock_rule_can_grant_a_command_only_its_own_proc_entry() {
    let l = layout();
    let bystander = Bystander::start();
    let own_pid_rule = |paths: Vec<std::path::PathBuf>| {
        let mut tool = shell(&l);
        tool.confine.as_mut().unwrap().read_exec.extend(paths);
        tool
    };

    // A rule on `/proc/self/auxv` resolves to the process that makes it.
    let tool = own_pid_rule(vec!["/proc/self/auxv".into()]);
    let out = sh(&tool, &l.workspace, &read_in_shell("/proc/self/auxv")).await;
    assert!(
        out.content.contains("Permission denied"),
        "a rule made ahead of time named the command's own pid: {}",
        out.content
    );

    // The rule that would help every command is the whole of `/proc`; it
    // shows the bystander's command line (not guarded by any ptrace check).
    let tool = own_pid_rule(vec!["/proc".into()]);
    let command = read_in_shell(&format!("/proc/{}/cmdline", bystander.pid()));
    let out = sh(&tool, &l.workspace, &command).await;
    assert!(
        out.content.contains("READ-OK"),
        "expected the blanket /proc grant to expose another process: {}",
        out.content
    );
}
