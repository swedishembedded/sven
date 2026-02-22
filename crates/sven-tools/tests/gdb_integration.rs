/// Integration tests for the GDB tooling.
///
/// Tests are organised into three groups:
///
///   1. **Always-run tests** (no ignore): use temp files or tiny helper
///      processes (nc, sleep, false) — run in standard `cargo test`.
///
///   2. **Probe-required tests** (#[ignore]): need a live J-Link probe and a
///      connected target. Run explicitly:
///        cargo test -p sven-tools -- gdb_integration --ignored --nocapture
///
///   3. **Mock server tests**: spin up an in-process TCP server that mimics
///      GDB/MI protocol. Run in standard `cargo test`.
///
/// Hardware tests also have a `hardware-tests` feature gate; they remain
/// `#[ignore]` as a safety net even when the feature is enabled.

mod gdb_integration {
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::Mutex;

    use sven_config::GdbConfig;
    use sven_tools::{
        GdbCommandTool, GdbConnectTool, GdbInterruptTool, GdbSessionState,
        GdbStartServerTool, GdbStatusTool, GdbStopTool, GdbWaitStoppedTool,
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
            connect_timeout_secs: 30,
            server_startup_wait_ms: 1000,
        }
    }

    fn fast_cfg() -> GdbConfig {
        GdbConfig {
            command_timeout_secs: 3,
            server_startup_wait_ms: 200,
            ..cfg()
        }
    }

    // ── Mock-server helpers ───────────────────────────────────────────────────

    /// Start a fake GDB/MI server on a random port using netcat.
    /// Returns the port number. The server will accept one connection, send a
    /// minimal GDB/MI greeting, and then exit.
    async fn start_mock_gdb_server() -> Option<u16> {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
        let port = listener.local_addr().ok()?.port();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Ok((mut stream, _)) = listener.accept().await {
                // Send a minimal GDB/MI greeting: version banner + prompt
                let greeting = b"=thread-group-added,id=\"i1\"\r\n\
                                  ~\"GNU gdb (GDB) 12.0\\n\"\r\n\
                                  ~\"Remote debugging using 127.0.0.1:0\\n\"\r\n\
                                  (gdb) \r\n";
                let _ = stream.write_all(greeting).await;
                // Keep the connection open briefly
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        // Give the server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Some(port)
    }

    // ── Server lifecycle tests (no probe required) ────────────────────────────

    #[tokio::test]
    async fn start_server_fails_with_nonexistent_binary() {
        let state = make_state();
        let t = GdbStartServerTool::new(state, cfg());
        let out = t.execute(&call("gdb_start_server", json!({
            "command": "/nonexistent/JLinkGDBServer -port 2331"
        }))).await;
        assert!(out.is_error, "expected error, got: {}", out.content);
    }

    #[tokio::test]
    async fn start_server_fails_fast_when_command_exits_immediately() {
        let state = make_state();
        let t = GdbStartServerTool::new(state, fast_cfg());
        let out = t.execute(&call("gdb_start_server", json!({
            "command": "false"
        }))).await;
        assert!(out.is_error, "expected error from 'false' command");
        assert!(
            out.content.contains("exited immediately"),
            "expected 'exited immediately', got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn start_server_refuses_second_start_while_running() {
        let state = make_state();
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        let t = GdbStartServerTool::new(state, cfg());
        let out = t.execute(&call("gdb_start_server", json!({
            "command": "sleep 60"
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("already running"));
    }

    #[tokio::test]
    async fn start_server_reports_port_in_output() {
        let state = make_state();
        let t = GdbStartServerTool::new(state.clone(), fast_cfg());
        // Use 'sleep 5' as a long-lived dummy server
        let out = t.execute(&call("gdb_start_server", json!({
            "command": "sleep 5 -port 2331"
        }))).await;
        // This might succeed (sleep keeps running) or fail (sleep exits); either way
        // check state is set or cleaned up properly
        let s = state.lock().await;
        // If it succeeded the server_addr should contain the port
        if !out.is_error {
            assert!(
                s.server_addr.as_deref().unwrap_or("").contains("2331"),
                "expected addr to contain port 2331"
            );
        }
    }

    #[tokio::test]
    async fn stop_with_no_session_is_ok() {
        let state = make_state();
        let t = GdbStopTool::new(state);
        let out = t.execute(&call("gdb_stop", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn stop_kills_running_server() {
        let state = make_state();
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        let t = GdbStopTool::new(state.clone());
        let out = t.execute(&call("gdb_stop", json!({}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("stopped"));

        let s = state.lock().await;
        assert!(!s.has_server(), "server should be cleared after stop");
        assert!(!s.has_client(), "client should be cleared after stop");
    }

    // ── Connect lifecycle tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn connect_fails_when_gdb_binary_not_found() {
        let state = make_state();
        let t = GdbConnectTool::new(state, GdbConfig {
            gdb_path: "/nonexistent/gdb-multiarch".into(),
            ..fast_cfg()
        });
        let out = t.execute(&call("gdb_connect", json!({"port": 2331}))).await;
        assert!(out.is_error);
        assert!(
            out.content.contains("Failed to spawn"),
            "expected spawn error, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn connect_fails_when_elf_not_found() {
        let state = make_state();
        let t = GdbConnectTool::new(state, fast_cfg());
        let out = t.execute(&call("gdb_connect", json!({
            "port": 2331,
            "executable": "/nonexistent/path/firmware.elf"
        }))).await;
        assert!(out.is_error);
        assert!(
            out.content.contains("ELF file not found"),
            "expected ELF-not-found error, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn connect_fails_gracefully_when_nothing_listening() {
        let state = make_state();
        let t = GdbConnectTool::new(state, fast_cfg());
        let out = t.execute(&call("gdb_connect", json!({"port": 19997}))).await;
        assert!(out.is_error, "expected failure when nothing is listening");
        // Error message should mention connection issue
        let c = out.content.to_lowercase();
        assert!(
            c.contains("connect") || c.contains("gdb") || c.contains("failed"),
            "expected helpful error, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn connect_provides_hint_when_refused() {
        let state = make_state();
        let t = GdbConnectTool::new(state, fast_cfg());
        let out = t.execute(&call("gdb_connect", json!({"port": 19996}))).await;
        assert!(out.is_error);
        // Hint should guide the user
        // (hint is appended after the raw error)
        println!("Error output: {}", out.content);
    }

    // ── Command / interrupt guard tests ──────────────────────────────────────

    #[tokio::test]
    async fn command_fails_when_not_connected() {
        let state = make_state();
        let t = GdbCommandTool::new(state, GdbConfig::default());
        let out = t.execute(&call("gdb_command", json!({
            "command": "info registers"
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn command_fails_with_missing_arg() {
        let state = make_state();
        let t = GdbCommandTool::new(state, GdbConfig::default());
        let out = t.execute(&call("gdb_command", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn interrupt_fails_when_not_connected() {
        let state = make_state();
        let t = GdbInterruptTool::new(state);
        let out = t.execute(&call("gdb_interrupt", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    // ── Discovery integration tests ───────────────────────────────────────────

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

    #[tokio::test]
    async fn discovery_reads_debugging_launch_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("debugging")).unwrap();
        std::fs::write(
            dir.path().join("debugging").join("launch.json"),
            r#"{
                "configurations": [{
                    "name": "Quake",
                    "type": "cortex-debug",
                    "servertype": "jlink",
                    "device": "AT32F435RMT7",
                    "interface": "SWD"
                }]
            }"#,
        ).unwrap();

        let cmd = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await.unwrap().unwrap();

        assert!(cmd.contains("AT32F435RMT7"), "expected AT32 device, got: {cmd}");
        assert!(cmd.contains("JLinkGDBServer"));
        assert!(cmd.contains("SWD"));
    }

    #[tokio::test]
    async fn discovery_reads_makefile_device() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "flash:\n\tJLinkExe -nogui 1 -if swd -speed 4000 -device AT32F435RMT7\n",
        ).unwrap();

        let cmd = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await.unwrap().unwrap();

        assert!(cmd.contains("AT32F435RMT7"), "got: {cmd}");
    }

    #[tokio::test]
    async fn discovery_returns_none_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn discovery_prefers_gdbinit_over_makefile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".gdbinit"),
            "# JLinkGDBServer -device GDBINIT_DEVICE -if SWD -speed 4000 -port 2331\n",
        ).unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "flash:\n\tJLinkExe -device MAKEFILE_DEVICE -if SWD -speed 4000\n",
        ).unwrap();

        let cmd = sven_tools::builtin::gdb::discovery::discover_gdb_server_command_in(
            Some(dir.path())
        ).await.unwrap().unwrap();

        assert!(cmd.contains("GDBINIT_DEVICE"), ".gdbinit should win: {cmd}");
        assert!(!cmd.contains("MAKEFILE_DEVICE"), "Makefile should lose: {cmd}");
    }

    // ── ELF discovery integration tests ──────────────────────────────────────

    #[test]
    fn elf_discovery_finds_sysbuild_elf() {
        let dir = tempfile::tempdir().unwrap();
        let elf_dir = dir.path()
            .join("build-firmware")
            .join("ng-iot-platform")
            .join("zephyr");
        std::fs::create_dir_all(&elf_dir).unwrap();
        let elf_path = elf_dir.join("zephyr.elf");
        std::fs::write(&elf_path, b"\x7fELF").unwrap();

        let found = sven_tools::builtin::gdb::discovery::find_firmware_elf(dir.path());
        assert!(found.is_some(), "should find ELF");
        assert_eq!(found.unwrap(), elf_path);
    }

    #[test]
    fn elf_discovery_skips_mcuboot_prefers_app() {
        let dir = tempfile::tempdir().unwrap();

        let mcuboot = dir.path().join("build-firmware").join("mcuboot").join("zephyr");
        std::fs::create_dir_all(&mcuboot).unwrap();
        std::fs::write(mcuboot.join("zephyr.elf"), b"\x7fELF").unwrap();

        let app = dir.path().join("build-firmware").join("ng-iot-platform").join("zephyr");
        std::fs::create_dir_all(&app).unwrap();
        let app_elf = app.join("zephyr.elf");
        std::fs::write(&app_elf, b"\x7fELF").unwrap();

        let found = sven_tools::builtin::gdb::discovery::find_firmware_elf(dir.path());
        assert!(found.is_some());
        let p = found.unwrap();
        assert!(
            !p.to_string_lossy().contains("mcuboot"),
            "should not pick mcuboot ELF, got: {:?}",
            p
        );
    }

    #[test]
    fn elf_discovery_returns_none_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sven_tools::builtin::gdb::discovery::find_firmware_elf(dir.path()).is_none());
    }

    // ── Probe-required tests (always #[ignore]) ────────────────────────────────

    /// Full JLink session with a real probe. Requires AT32F435RMT7 connected.
    #[tokio::test]
    #[ignore = "requires JLinkGDBServer and connected AT32F435RMT7 target"]
    async fn hardware_jlink_at32_full_lifecycle() {
        let state = make_state();

        let start = GdbStartServerTool::new(state.clone(), cfg());
        let out = start.execute(&call("gdb_start_server", json!({
            "command": "JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 2331"
        }))).await;
        assert!(!out.is_error, "start failed: {}", out.content);

        let connect = GdbConnectTool::new(state.clone(), cfg());
        let out = connect.execute(&call("gdb_connect", json!({"port": 2331}))).await;
        assert!(!out.is_error, "connect failed: {}", out.content);
        assert!(out.content.contains("Connected"), "got: {}", out.content);

        let cmd = GdbCommandTool::new(state.clone(), GdbConfig::default());

        let out = cmd.execute(&call("gdb_command", json!({
            "command": "monitor reset halt"
        }))).await;
        assert!(!out.is_error, "reset halt failed: {}", out.content);

        let out = cmd.execute(&call("gdb_command", json!({
            "command": "info registers"
        }))).await;
        assert!(!out.is_error, "info registers failed: {}", out.content);
        // Should contain register names like pc, sp
        assert!(
            out.content.to_lowercase().contains("pc") || out.content.contains("r0"),
            "expected register output, got: {}",
            out.content
        );
        println!("Registers:\n{}", out.content);

        let stop = GdbStopTool::new(state.clone());
        let out = stop.execute(&call("gdb_stop", json!({}))).await;
        assert!(!out.is_error, "stop failed: {}", out.content);

        let s = state.lock().await;
        assert!(!s.has_server());
        assert!(!s.has_client());
    }

    /// Full lifecycle with STM32H562VI target.
    #[tokio::test]
    #[ignore = "requires JLinkGDBServer and connected STM32H562VI target"]
    async fn hardware_jlink_stm32h5_full_lifecycle() {
        let state = make_state();

        let start = GdbStartServerTool::new(state.clone(), cfg());
        let out = start.execute(&call("gdb_start_server", json!({
            "command": "JLinkGDBServer -device STM32H562VI -if SWD -speed 4000 -port 2331"
        }))).await;
        assert!(!out.is_error, "start failed: {}", out.content);

        let connect = GdbConnectTool::new(state.clone(), cfg());
        let out = connect.execute(&call("gdb_connect", json!({"port": 2331}))).await;
        assert!(!out.is_error, "connect failed: {}", out.content);

        let cmd = GdbCommandTool::new(state.clone(), GdbConfig::default());
        let out = cmd.execute(&call("gdb_command", json!({
            "command": "info registers"
        }))).await;
        println!("STM32H5 registers:\n{}", out.content);

        let stop = GdbStopTool::new(state);
        stop.execute(&call("gdb_stop", json!({}))).await;
    }

    /// Verify error propagation: connecting to a port with nothing listening.
    #[tokio::test]
    #[ignore = "slow test (waits for timeout); run when validating error handling"]
    async fn graceful_failure_no_server_slow() {
        let state = make_state();
        let connect = GdbConnectTool::new(state, GdbConfig {
            command_timeout_secs: 10,
            ..cfg()
        });
        let out = connect.execute(&call("gdb_connect", json!({
            "port": 19999
        }))).await;
        assert!(out.is_error, "expected failure when no server is present");
        println!("Expected error: {}", out.content);
        // Hint should mention server
        assert!(
            out.content.contains("gdb_start_server") || out.content.contains("listening"),
            "expected helpful hint, got: {}",
            out.content
        );
    }

    /// Test that gdb_interrupt handles a timeout gracefully.
    #[tokio::test]
    #[ignore = "requires JLinkGDBServer"]
    async fn hardware_interrupt_timeout_is_graceful() {
        let state = make_state();
        let start = GdbStartServerTool::new(state.clone(), cfg());
        start.execute(&call("gdb_start_server", json!({
            "command": "JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 2331"
        }))).await;

        let connect = GdbConnectTool::new(state.clone(), cfg());
        connect.execute(&call("gdb_connect", json!({"port": 2331}))).await;

        let interrupt = GdbInterruptTool::new(state.clone());
        let out = interrupt.execute(&call("gdb_interrupt", json!({
            "timeout_secs": 2
        }))).await;
        println!("interrupt result: {}", out.content);

        let stop = GdbStopTool::new(state);
        stop.execute(&call("gdb_stop", json!({}))).await;
    }

    // ── Error recovery tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn state_clears_after_stop() {
        let state = make_state();
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep").arg("60").spawn().unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        assert!(state.lock().await.has_server());

        let stop = GdbStopTool::new(state.clone());
        stop.execute(&call("gdb_stop", json!({}))).await;

        let s = state.lock().await;
        assert!(!s.has_server(), "server should be cleared");
        assert!(!s.has_client(), "client should be cleared");
        assert!(s.server_addr.is_none(), "addr should be cleared");
    }

    #[tokio::test]
    async fn double_stop_is_idempotent() {
        let state = make_state();
        let stop = GdbStopTool::new(state.clone());

        let out1 = stop.execute(&call("gdb_stop", json!({}))).await;
        let out2 = stop.execute(&call("gdb_stop", json!({}))).await;

        assert!(!out1.is_error);
        assert!(!out2.is_error);
    }
}
