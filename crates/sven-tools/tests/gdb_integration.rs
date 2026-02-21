/// Integration tests for the GDB tooling.
///
/// These tests require `gdb-multiarch` (or `gdb`) and a functional debug probe
/// to be present on the machine, so they are marked `#[ignore]` and must be
/// run explicitly:
///
///   cargo test -p sven-tools -- gdb_integration --ignored --nocapture
///
/// They serve as manual regression tests when the full embedded toolchain is
/// available, and as documentation for how the tools compose.

#[cfg(test)]
mod gdb_integration {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::Mutex;

    use sven_config::GdbConfig;
    use sven_tools::{
        GdbCommandTool, GdbConnectTool, GdbInterruptTool, GdbSessionState,
        GdbStartServerTool, GdbStopTool,
    };
    use sven_tools::tool::{Tool, ToolCall};

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: "test".into(), name: name.into(), args }
    }

    fn make_state() -> Arc<Mutex<GdbSessionState>> {
        Arc::new(Mutex::new(GdbSessionState::default()))
    }

    fn cfg() -> GdbConfig {
        GdbConfig {
            gdb_path: "gdb-multiarch".into(),
            command_timeout_secs: 15,
            server_startup_wait_ms: 1000,
        }
    }

    /// Verify that gdb_start_server correctly detects port extraction for a
    /// JLink command and that the process starts (requires JLinkGDBServer).
    #[tokio::test]
    #[ignore]
    async fn start_jlink_server_and_connect() {
        let state = make_state();

        // ── 1. Start server ──────────────────────────────────────────────────
        let start = GdbStartServerTool::new(state.clone(), cfg());
        let out = start.execute(&call("gdb_start_server", json!({
            "command": "JLinkGDBServer -device cortex-m4 -if SWD -speed 4000 -port 2331"
        }))).await;
        assert!(!out.is_error, "start_server failed: {}", out.content);
        assert!(out.content.contains("2331"), "expected port in output");

        // ── 2. Connect ───────────────────────────────────────────────────────
        let connect = GdbConnectTool::new(state.clone(), cfg());
        let out = connect.execute(&call("gdb_connect", json!({
            "port": 2331
        }))).await;
        assert!(!out.is_error, "connect failed: {}", out.content);

        // ── 3. Run a simple command ──────────────────────────────────────────
        let command = GdbCommandTool::new(state.clone());
        let out = command.execute(&call("gdb_command", json!({
            "command": "info target"
        }))).await;
        assert!(!out.is_error, "info target failed: {}", out.content);
        println!("info target:\n{}", out.content);

        // ── 4. Stop ──────────────────────────────────────────────────────────
        let stop = GdbStopTool::new(state.clone());
        let out = stop.execute(&call("gdb_stop", json!({}))).await;
        assert!(!out.is_error, "stop failed: {}", out.content);

        // State should be cleared
        let s = state.lock().await;
        assert!(!s.has_server());
        assert!(!s.has_client());
    }

    /// Test the complete lifecycle without a real debug probe by using
    /// `openocd` with a dummy target (if openocd is installed).
    #[tokio::test]
    #[ignore]
    async fn openocd_lifecycle() {
        let state = make_state();
        let c = GdbConfig {
            server_startup_wait_ms: 1500,
            command_timeout_secs: 10,
            ..cfg()
        };

        let start = GdbStartServerTool::new(state.clone(), c.clone());
        let out = start.execute(&call("gdb_start_server", json!({
            "command": "openocd -f board/stm32f4discovery.cfg"
        }))).await;
        assert!(!out.is_error, "openocd start failed: {}", out.content);

        let connect = GdbConnectTool::new(state.clone(), c);
        let out = connect.execute(&call("gdb_connect", json!({
            "port": 3333
        }))).await;
        assert!(!out.is_error, "connect failed: {}", out.content);

        let stop = GdbStopTool::new(state);
        let out = stop.execute(&call("gdb_stop", json!({}))).await;
        assert!(!out.is_error, "stop failed: {}", out.content);
    }

    /// Verify error propagation: connecting to a port with nothing listening.
    #[tokio::test]
    #[ignore]
    async fn connect_fails_gracefully_when_no_server() {
        let state = make_state();
        // Nothing is listening on port 19999
        let connect = GdbConnectTool::new(state, GdbConfig {
            command_timeout_secs: 3,
            ..cfg()
        });
        let out = connect.execute(&call("gdb_connect", json!({
            "port": 19999
        }))).await;
        // Should fail gracefully, not panic
        assert!(out.is_error, "expected failure when no server is present");
        println!("Expected error: {}", out.content);
    }

    /// Test that gdb_interrupt sends the interrupt command and handles a timeout
    /// gracefully when there is no running target (uses a short timeout).
    #[tokio::test]
    #[ignore]
    async fn interrupt_timeout_is_graceful() {
        let state = make_state();
        let start = GdbStartServerTool::new(state.clone(), cfg());
        start.execute(&call("gdb_start_server", json!({
            "command": "JLinkGDBServer -device cortex-m4 -if SWD -speed 4000 -port 2331"
        }))).await;

        let connect = GdbConnectTool::new(state.clone(), cfg());
        connect.execute(&call("gdb_connect", json!({"port": 2331}))).await;

        // Interrupt when already stopped → should succeed or timeout gracefully
        let interrupt = GdbInterruptTool::new(state.clone());
        let out = interrupt.execute(&call("gdb_interrupt", json!({
            "timeout_secs": 2
        }))).await;
        println!("interrupt result: {}", out.content);

        let stop = GdbStopTool::new(state);
        stop.execute(&call("gdb_stop", json!({}))).await;
    }

    /// Smoke-test discovery with a temporary .gdbinit file.
    #[tokio::test]
    async fn discovery_reads_gdbinit_comment() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let gdbinit_path = dir.path().join(".gdbinit");
        let mut f = std::fs::File::create(&gdbinit_path).unwrap();
        writeln!(
            f,
            "# JLinkGDBServer -device STM32F407VG -if SWD -speed 4000 -port 2331\ntarget remote :2331"
        ).unwrap();

        let result = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await;

        let cmd = result.unwrap().unwrap();
        assert!(cmd.contains("JLinkGDBServer"), "expected JLink command, got: {cmd}");
        assert!(cmd.contains("STM32F407VG"));
    }

    /// Discovery returns None when no hints are found.
    #[tokio::test]
    async fn discovery_returns_none_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await;
        assert!(result.unwrap().is_none());
    }
}
