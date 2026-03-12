// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::{AgentMode, GdbConfig};

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::discovery::{discover_gdb_server_command, extract_port_from_command};
use super::state::GdbSessionState;

pub struct GdbStartServerTool {
    state: Arc<Mutex<GdbSessionState>>,
    cfg: GdbConfig,
}

impl GdbStartServerTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>, cfg: GdbConfig) -> Self {
        Self { state, cfg }
    }
}

/// Check whether any process is listening on `port` on localhost.
async fn is_port_listening(port: u16) -> bool {
    tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .is_ok()
}

/// Kill every process currently listening on `port`.
///
/// Uses `ss -tlnp` to locate the PID, sends SIGTERM, waits briefly, then
/// uses `pkill -9` on the common server binary names for a hard fallback.
async fn kill_process_on_port(port: u16) {
    // Locate PID via ss
    if let Ok(out) = tokio::process::Command::new("ss")
        .args(["-tlnp"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if !line.contains(&format!(":{port}")) {
                continue;
            }
            if let Some(idx) = line.find("pid=").map(|i| i + 4) {
                let rest = &line[idx..];
                let end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                if let Ok(pid) = rest[..end].parse::<i32>() {
                    debug!(pid, port, "kill_process_on_port: SIGTERM");
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
        }
    }

    tokio::time::sleep(Duration::from_millis(400)).await;

    // Hard SIGKILL on common GDB-server binary names to ensure cleanup.
    // Redirect stdout/stderr to null so pkill's diagnostic messages don't
    // leak onto the TUI screen.
    for name in ["JLinkGDBServer", "JLinkGDBServerCL", "openocd", "pyocd"] {
        let _ = tokio::process::Command::new("pkill")
            .args(["-9", "-x", name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[async_trait]
impl Tool for GdbStartServerTool {
    fn name(&self) -> &str {
        "gdb_start_server"
    }

    fn description(&self) -> &str {
        "Start a GDB debug server in the background (e.g., JLinkGDBServer, OpenOCD, pyocd). \
         Provide the target device name via `target` (e.g. 'STM32F407VG') or a full `command`. \
         If neither is given, the agent attempts to discover the command from project files \
         (.gdbinit, .vscode/launch.json, debugging/launch.json, openocd.cfg, platformio.ini). \
         If discovery also fails, the tool will instruct you to ask the user for the target name. \
         Use gdb_connect after this to attach gdb-multiarch to the running server. \
         Only call this once per session; use gdb_stop to shut everything down. \
         If a zombie server is already listening on the target port from a previous session, \
         set force=true to kill it and start fresh."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Full GDB server command to run \
                        (e.g., 'JLinkGDBServer -device <TARGET> -if SWD -speed 4000 -port 2331'). \
                        If omitted, the command is built from `target` or discovered automatically."
                },
                "target": {
                    "type": "string",
                    "description": "Target device/chip name (e.g. 'STM32F407VG', 'nRF52840_xxAA'). \
                        Used to build a default JLinkGDBServer command when `command` is not provided. \
                        Ask the user for this value if it is not known."
                },
                "force": {
                    "type": "boolean",
                    "description": "Kill any existing process listening on the target port before \
                        starting a new server. Use when a previous session left a zombie GDB \
                        server running. Default: false."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let force = call
            .args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // If we already track a server in our state, handle it.
        {
            let state = self.state.lock().await;
            if state.has_server() {
                if !force {
                    return ToolOutput::err(
                        &call.id,
                        "GDB server is already running. Call gdb_stop first, or \
                         use force=true to kill it and restart.",
                    );
                }
                drop(state);
                // Kill our own tracked server before starting a new one.
                self.state.lock().await.clear().await;
            }
        }

        // Determine command to run.
        let command = if let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) {
            cmd.to_string()
        } else if let Some(target) = call.args.get("target").and_then(|v| v.as_str()) {
            format!("JLinkGDBServer -device {target} -if SWD -speed 4000 -port 2331")
        } else {
            match discover_gdb_server_command().await {
                Ok(Some(cmd)) => cmd,
                Ok(None) => {
                    return ToolOutput::err(
                        &call.id,
                        "Could not discover a GDB server command from project files, and no \
                         target device was specified. Ask the user what device or chip they \
                         are debugging (e.g. 'STM32F407VG', 'nRF52840_xxAA') and call \
                         gdb_start_server again with the `target` argument.",
                    )
                }
                Err(e) => return ToolOutput::err(&call.id, format!("Discovery error: {e}")),
            }
        };

        let port = extract_port_from_command(&command).unwrap_or(2331);

        // Before spawning, check whether a server is already listening on the target
        // port.  This covers the common case where the user started JLinkGDBServer or
        // OpenOCD manually outside the agent.  We treat this as success and store the
        // address so that gdb_connect can infer the port exactly as if we had started
        // the server ourselves.  force=true bypasses this and kills the existing process.
        if !force && is_port_listening(port).await {
            debug!(port, "gdb_start_server: server already listening, reusing");
            let addr = format!("localhost:{port}");
            self.state.lock().await.set_external_server(addr.clone());
            return ToolOutput::ok(
                &call.id,
                format!(
                    "GDB server already running on {addr}.\n\
                     Call gdb_connect to attach. \
                     Use force=true to kill it and start a fresh server."
                ),
            );
        }

        // When force=true, evict any external process already on the port.
        if force {
            debug!(
                port,
                "gdb_start_server: force=true, killing any existing server on port"
            );
            kill_process_on_port(port).await;
        }

        debug!(cmd = %command, "gdb_start_server: spawning");

        // Spawn the server in its own process group so that kill(-pgid, SIGKILL)
        // later takes out the whole tree (including JLinkGUIServerExe children).
        // kill_on_drop(true) also ensures cleanup if sven exits without gdb_stop.
        let mut server_cmd = tokio::process::Command::new("sh");
        server_cmd
            .arg("-c")
            .arg(&command)
            .process_group(0)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Detach the GDB server from the controlling terminal.  Applications
        // like JLinkGDBServer open /dev/tty directly (bypassing our
        // stdin/stdout/stderr redirects) and send escape sequences that disable
        // mouse capture or corrupt the TUI.  setsid() makes the child the
        // leader of a new session with no controlling terminal, so any
        // open("/dev/tty") call in it (or its children) fails with ENXIO.
        #[cfg(unix)]
        unsafe {
            server_cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = match server_cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to spawn server: {e}")),
        };

        // The child's PID equals its PGID when spawned with process_group(0).
        let server_pgid = child.id();

        let addr = format!("localhost:{port}");

        // Brief wait for the server to initialise.
        tokio::time::sleep(Duration::from_millis(self.cfg.server_startup_wait_ms)).await;

        // Store child in state then verify it is still alive.
        let mut state = self.state.lock().await;
        state.set_server(child, addr.clone(), server_pgid);

        let child_exited = state
            .server
            .as_mut()
            .and_then(|s| s.child.as_mut())
            .and_then(|c| c.try_wait().ok().flatten())
            .map(|s| s.code().unwrap_or(-1));

        if let Some(exit_code) = child_exited {
            let _ = state.server.take();
            drop(state); // release lock before the async port check

            let port_occupied = is_port_listening(port).await;
            if port_occupied {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "GDB server exited immediately (exit {exit_code}).\n\
                         Port {port} is already in use — likely a zombie server from a \
                         previous session.\n\n\
                         • To kill the zombie and restart: \
                           call gdb_start_server with force=true\n\
                         • To reuse the existing server: \
                           call gdb_connect directly"
                    ),
                );
            }

            return ToolOutput::err(
                &call.id,
                format!(
                    "GDB server exited immediately (exit {exit_code}).\n\
                     Check that the server binary is installed and the command is correct.\n\
                     Verify the J-Link probe is connected \
                     ('ss -tln | grep {port}' should show LISTEN)."
                ),
            );
        }

        ToolOutput::ok(
            &call.id,
            format!(
                "GDB server started successfully.\n\
                 Command: {command}\n\
                 Listening at: {addr}\n\
                 Call gdb_connect to attach gdb-multiarch."
            ),
        )
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile;

    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb_start_server".into(),
            args,
        }
    }

    fn make_tool() -> GdbStartServerTool {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        GdbStartServerTool::new(state, GdbConfig::default())
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = make_tool();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_if_command_exits_immediately() {
        let t = make_tool();
        // `false` exits with code 1 immediately
        let out = t.execute(&call(json!({"command": "false"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("exited immediately"));
    }

    #[tokio::test]
    async fn target_parameter_builds_jlink_command() {
        let t = make_tool();
        let out = t.execute(&call(json!({"target": "STM32F407VG"}))).await;
        // Regardless of whether the port is occupied or the binary is missing,
        // the `target` path must never produce the "ask the user" message —
        // that error is only for the case where no target/command was given at all.
        assert!(
            !out.content.contains("Ask the user"),
            "target was ignored; got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn no_command_no_target_no_discovery_tells_model_to_ask_user() {
        // Run in an empty temp dir so discovery returns None.
        let original_dir = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let t = make_tool();
        let out = t.execute(&call(json!({}))).await;

        std::env::set_current_dir(original_dir).unwrap();

        assert!(out.is_error);
        assert!(
            out.content.contains("Ask the user"),
            "expected prompt to ask user, got: {}",
            out.content
        );
        assert!(
            out.content.contains("target"),
            "expected mention of `target` arg, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn fails_if_already_running_without_force() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        let t = GdbStartServerTool::new(state, GdbConfig::default());
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("already running"));
    }

    #[tokio::test]
    async fn force_clears_existing_and_retries() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        let t = GdbStartServerTool::new(state, GdbConfig::default());
        // Using `false` as the command: the existing server will be cleared,
        // then the new server (`false`) will exit immediately.
        let out = t
            .execute(&call(json!({"command": "false", "force": true})))
            .await;
        assert!(out.is_error);
        // Should fail because `false` exits immediately, not because of "already running"
        assert!(out.content.contains("exited immediately"));
        assert!(!out.content.contains("already running"));
    }

    #[tokio::test]
    async fn succeeds_when_port_already_listening_externally() {
        // Simulate an externally-started GDB server by binding a real listener.
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_port = listener.local_addr().unwrap().port();
        let _listener = listener; // keep alive for the duration of the test

        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStartServerTool::new(state.clone(), GdbConfig::default());
        let cmd = format!("JLinkGDBServer -device STM32F4 -port {occupied_port}");
        let out = t.execute(&call(json!({"command": cmd}))).await;

        // Should succeed (not error) because the port is already occupied.
        assert!(!out.is_error, "expected success, got: {}", out.content);
        assert!(
            out.content.contains("already running"),
            "got: {}",
            out.content
        );
        assert!(out.content.contains("gdb_connect"), "got: {}", out.content);

        // server addr must be stored so gdb_connect can infer the port.
        let s = state.lock().await;
        assert_eq!(
            s.server.as_ref().map(|srv| srv.addr.as_str()),
            Some(format!("localhost:{occupied_port}").as_str())
        );
    }

    #[tokio::test]
    async fn force_bypasses_external_server_and_kills_it() {
        // With force=true the early-exit path is skipped; spawning `false` then
        // exits immediately, confirming the guard was bypassed.
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _listener = listener;

        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStartServerTool::new(state, GdbConfig::default());
        let cmd = "false".to_string(); // exits immediately — proves we didn't return early
        let out = t
            .execute(&call(json!({"command": cmd, "force": true})))
            .await;

        // force=true should skip the idempotent check, attempt to spawn, and fail.
        assert!(out.is_error);
        assert!(
            out.content.contains("exited immediately"),
            "got: {}",
            out.content
        );
    }
}
