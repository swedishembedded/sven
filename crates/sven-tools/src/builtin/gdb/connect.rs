// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;
#[cfg(unix)]
use libc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::{AgentMode, GdbConfig};

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

pub struct GdbConnectTool {
    state: Arc<Mutex<GdbSessionState>>,
    cfg: GdbConfig,
}

impl GdbConnectTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>, cfg: GdbConfig) -> Self {
        Self { state, cfg }
    }
}

#[async_trait]
impl Tool for GdbConnectTool {
    fn name(&self) -> &str { "gdb_connect" }

    fn description(&self) -> &str {
        "Spawn gdb-multiarch and connect it to a running GDB server. \
         If gdb_start_server was called previously the port is inferred automatically. \
         Supply the ELF binary path via 'executable' so GDB loads debug symbols. \
         Uses 'target extended-remote' for robust connection to JLink/OpenOCD servers. \
         After connecting, use gdb_command to run debugger commands.\n\
         Example:\n\
           gdb_connect({\"executable\": \"build/zephyr.elf\", \"port\": 2331})"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "host": {
                    "type": "string",
                    "description": "GDB server host (default: 'localhost')"
                },
                "port": {
                    "type": "integer",
                    "description": "GDB server port. Inferred from gdb_start_server if omitted."
                },
                "executable": {
                    "type": "string",
                    "description": "Path to the ELF binary for debug symbol loading. \
                        Required for meaningful debugging (info registers, breakpoints, etc.)."
                },
                "gdb_path": {
                    "type": "string",
                    "description": "Path or name of the GDB executable to use \
                        (default from config, typically 'gdb-multiarch')."
                }
            },
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let mut state = self.state.lock().await;

        if state.has_client() {
            return ToolOutput::err(
                &call.id,
                "Already connected to a GDB session. Use gdb_stop to end it first.",
            );
        }

        // Resolve target address
        let host = call.args
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("localhost")
            .to_string();

        let port: u16 = if let Some(p) = call.args.get("port").and_then(|v| v.as_u64()) {
            p as u16
        } else if let Some(addr) = &state.server_addr {
            // Parse port from "host:port"
            addr.split(':').next_back()
                .and_then(|p| p.parse().ok())
                .unwrap_or(2331)
        } else {
            2331
        };

        let target_addr = format!("{host}:{port}");
        let gdb_path = call.args
            .get("gdb_path")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.cfg.gdb_path)
            .to_string();
        let executable = call.args
            .get("executable")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Validate ELF path if provided
        if let Some(exe) = &executable {
            if !std::path::Path::new(exe).exists() {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "ELF file not found: {exe}\n\
                         Build the firmware first, then provide the correct path."
                    ),
                );
            }
        }

        debug!(target = %target_addr, gdb = %gdb_path, "gdb_connect: probing server reachability");

        // ── Step 1: Probe server reachability ────────────────────────────────
        // Spawn a short-lived synchronous GDB process with -ex to verify the
        // server is reachable.  We read its MI output until we see a success or
        // failure signal, then kill it.  This gives us fast, reliable feedback
        // before we spawn the long-lived async client.
        //
        // We use `std::process::Command` (not tokio) so we can take ownership of
        // stdout and read it with a blocking-thread timeout — no async executor
        // involvement, no gdbmi internals to fight.
        let probe_output = probe_server_connection(&gdb_path, &target_addr, &executable,
            Duration::from_secs(self.cfg.command_timeout_secs));

        match probe_output {
            ProbeResult::Failed(output) => {
                let hint = connection_error_hint(&output, &target_addr);
                return ToolOutput::err(
                    &call.id,
                    format!("Failed to connect to {target_addr}:\n{output}\n\n{hint}"),
                );
            }
            ProbeResult::SpawnError(e) => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Failed to spawn {gdb_path}: {e}\n\
                         Is gdb-multiarch installed? Try: apt-get install gdb-multiarch"
                    ),
                );
            }
            ProbeResult::Connected(_output) => {
                // Server is reachable; proceed to spawn the long-lived async client.
            }
        }

        debug!(target = %target_addr, "gdb_connect: server reachable, spawning async client");

        // ── Step 2: Spawn the long-lived async GDB/MI client ─────────────────
        // The probe confirmed the server is up.  Now spawn the real tokio
        // process with -ex so GDB connects during startup.  Passing the
        // connection as -ex makes GDB emit *stopped before the first (gdb)
        // prompt, which gdbmi::await_ready() detects correctly.
        let mut cmd = tokio::process::Command::new(&gdb_path);
        cmd.arg("--interpreter=mi3")
            .arg("--quiet")
            .arg("-nx");

        if let Some(exe) = &executable {
            cmd.arg(exe);
        }

        cmd.arg("-ex")
            .arg(format!("target extended-remote {target_addr}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Detach from the controlling terminal so gdb cannot open /dev/tty
        // and corrupt TUI state.
        #[cfg(unix)]
        unsafe { cmd.pre_exec(|| { libc::setsid(); Ok(()) }); }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(
                &call.id,
                format!(
                    "Failed to spawn {gdb_path} (async client): {e}\n\
                     Is gdb-multiarch installed? Try: apt-get install gdb-multiarch"
                ),
            ),
        };

        // Capture PID before handing the child to gdbmi, since gdbmi does not
        // expose the child PID after construction.  We need it to send SIGINT
        // for reliable hardware interrupts.
        let gdb_pid = child.id();

        // Use the connect timeout (longer) for the startup handshake.
        // Loading debug symbols from a large ELF can take 15-30s; using the
        // short command_timeout_secs (10s default) would cause a false timeout.
        let connect_timeout = Duration::from_secs(self.cfg.connect_timeout_secs);
        let command_timeout = Duration::from_secs(self.cfg.command_timeout_secs);

        let mut gdb = gdbmi::Gdb::new(child, connect_timeout);

        // await_ready() waits for GDB to emit (gdb) or a *stopped record.
        // With -ex "target extended-remote ...", GDB connects and emits *stopped
        // during startup, so await_ready() succeeds where it previously timed out
        // (the original bug: await_ready() was called on a fresh GDB with no -ex,
        // so *stopped was never emitted and the wait timed out).
        if let Err(e) = gdb.await_ready().await {
            return ToolOutput::err(
                &call.id,
                format!(
                    "GDB client ready timeout: {e}\n\
                     The server was reachable but GDB took too long to initialise.\n\
                     Hint: large ELFs (>10MB of symbols) can take 15-30s to load.\n\
                     → Increase connect_timeout_secs in your sven config (current: {}s).",
                    self.cfg.connect_timeout_secs
                ),
            );
        }

        // Switch to the shorter per-command timeout for normal operations.
        gdb.set_timeout(command_timeout);

        state.set_client(gdb, gdb_pid);

        ToolOutput::ok(
            &call.id,
            format!(
                "Connected to GDB server at {target_addr}.\n\
                 GDB executable: {gdb_path}\n\
                 {}Use gdb_command to run debugger commands.",
                executable
                    .map(|e| format!("Symbols loaded from: {e}\n"))
                    .unwrap_or_default(),
            ),
        )
    }
}

// ─── Probe helpers ────────────────────────────────────────────────────────────

enum ProbeResult {
    Connected(String),
    Failed(String),
    SpawnError(String),
}

/// Spawn a short-lived synchronous GDB process to test server reachability.
///
/// Uses `std::process::Command` (blocking) rather than tokio so we can read
/// stdout with a simple channel + timeout without holding an async executor.
/// The process is always killed when this function returns.
fn probe_server_connection(
    gdb_path: &str,
    target_addr: &str,
    executable: &Option<String>,
    timeout: Duration,
) -> ProbeResult {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    let mut cmd = Command::new(gdb_path);
    cmd.arg("--interpreter=mi3")
        .arg("--quiet")
        .arg("-nx");

    if let Some(exe) = executable {
        cmd.arg(exe);
    }

    cmd.arg("-ex")
        .arg(format!("target extended-remote {target_addr}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ProbeResult::SpawnError(e.to_string()),
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            return ProbeResult::SpawnError("Failed to capture GDB stdout".into());
        }
    };

    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + timeout;
    let mut lines: Vec<String> = Vec::new();
    let mut success = false;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) => {
                lines.push(line.clone());
                let lc = line.to_lowercase();

                // Success: GDB/MI emits these when extended-remote succeeds
                if line.contains("Remote debugging using")
                    || line.starts_with("*stopped")
                    || line.starts_with("^done")
                {
                    success = true;
                    break;
                }

                // Failure: explicit error records
                if line.starts_with("^error")
                    || (line.starts_with("&\"") && lc.contains("connection refused"))
                    || (line.starts_with("&\"") && lc.contains("timed out"))
                    || (line.starts_with("&\"") && lc.contains("no route"))
                {
                    success = false;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Always kill the probe process before returning
    let _ = child.kill();
    let _ = child.wait();

    let output = decode_mi_output(&lines);

    if success {
        ProbeResult::Connected(output)
    } else if lines.is_empty() {
        ProbeResult::Failed(format!("No output from GDB within {:.0}s", timeout.as_secs_f32()))
    } else {
        ProbeResult::Failed(output)
    }
}

/// Decode MI stream escape sequences for readable output.
fn decode_mi_output(lines: &[String]) -> String {
    lines
        .iter()
        .filter_map(|l| {
            if let Some(inner) = l.strip_prefix("~\"").and_then(|s| s.strip_suffix('"')) {
                Some(inner.replace("\\n", "\n").replace("\\\"", "\""))
            } else if let Some(inner) = l.strip_prefix("&\"").and_then(|s| s.strip_suffix('"')) {
                Some(inner.replace("\\n", "\n").replace("\\\"", "\""))
            } else if !l.starts_with('=') {
                Some(l.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

// ─── Error hint ───────────────────────────────────────────────────────────────

/// Return a human-readable hint based on the connection error output.
fn connection_error_hint(output: &str, target_addr: &str) -> String {
    let lower = output.to_lowercase();
    let port = target_addr.split(':').next_back().unwrap_or("2331");
    if lower.contains("connection refused") {
        format!(
            "Hint: Nothing is listening on {target_addr}.\n\
             → Call gdb_start_server first, or check that the GDB server is running.\n\
             → Verify with: ss -tln | grep {port}"
        )
    } else if lower.contains("timed out") || lower.contains("timeout") || output.contains("within") {
        format!(
            "Hint: Connection to {target_addr} timed out.\n\
             → The GDB server may still be initialising — retry gdb_connect in a moment.\n\
             → Increase command_timeout_secs in your sven config (current default: 10s).\n\
             → Check that the target device is powered and connected."
        )
    } else if lower.contains("no such file") {
        "Hint: ELF file not found. Ensure the firmware is built before connecting.".to_string()
    } else {
        format!(
            "Hint: Check that:\n\
             1. The GDB server (JLinkGDBServer/openocd) is running on {target_addr}\n\
             2. The target device is connected and powered\n\
             3. The correct device name is used in gdb_start_server"
        )
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "gdb_connect".into(), args }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbConnectTool::new(state, GdbConfig::default());
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_when_gdb_binary_not_found() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbConnectTool::new(state, GdbConfig {
            gdb_path: "/nonexistent/gdb-multiarch".into(),
            ..GdbConfig::default()
        });
        let out = t.execute(&call(json!({"port": 9999}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn fails_when_elf_not_found() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbConnectTool::new(state, GdbConfig::default());
        let out = t.execute(&call(json!({
            "port": 2331,
            "executable": "/nonexistent/firmware.elf"
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("ELF file not found"));
    }

    #[test]
    fn connection_refused_hint_is_helpful() {
        let hint = connection_error_hint("connection refused", "localhost:2331");
        assert!(hint.contains("gdb_start_server"));
        assert!(hint.contains("2331"));
    }

    #[test]
    fn timeout_hint_mentions_config() {
        let hint = connection_error_hint("timed out waiting", "localhost:2331");
        assert!(hint.contains("command_timeout_secs"));
    }

    #[test]
    fn generic_hint_covers_main_cases() {
        let hint = connection_error_hint("unknown error xyz", "localhost:2331");
        assert!(hint.contains("GDB server"));
        assert!(hint.contains("localhost:2331"));
    }

    #[tokio::test]
    async fn fails_gracefully_when_nothing_listening() {
        // Nothing listening on port 19998 — probe returns quickly with connection refused.
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbConnectTool::new(state, GdbConfig {
            command_timeout_secs: 5,
            ..GdbConfig::default()
        });
        let out = t.execute(&call(json!({"port": 19998}))).await;
        assert!(out.is_error, "expected failure when nothing is listening");
        let c = out.content.to_lowercase();
        assert!(
            c.contains("connect") || c.contains("gdb") || c.contains("failed"),
            "expected helpful error, got: {}",
            out.content
        );
    }

    #[test]
    fn decode_mi_output_strips_tilde_prefix() {
        let lines = vec![
            r#"~"Remote debugging using localhost:2331\n""#.to_string(),
            r#"*stopped,reason="signal-received""#.to_string(),
        ];
        let out = decode_mi_output(&lines);
        assert!(out.contains("Remote debugging"), "got: {out}");
    }

    #[test]
    fn decode_mi_output_strips_ampersand_prefix() {
        let lines = vec![
            r#"&"Connection refused.\n""#.to_string(),
        ];
        let out = decode_mi_output(&lines);
        assert!(out.contains("Connection refused"), "got: {out}");
    }

    #[test]
    fn probe_fails_for_nonexistent_gdb() {
        let result = probe_server_connection(
            "/nonexistent/gdb",
            "localhost:2331",
            &None,
            Duration::from_secs(2),
        );
        assert!(matches!(result, ProbeResult::SpawnError(_)));
    }

    #[test]
    fn probe_fails_when_nothing_listening() {
        let result = probe_server_connection(
            "gdb-multiarch",
            "localhost:19997",
            &None,
            Duration::from_secs(5),
        );
        // Should fail because nothing is listening
        assert!(matches!(result, ProbeResult::Failed(_) | ProbeResult::SpawnError(_)));
    }
}
