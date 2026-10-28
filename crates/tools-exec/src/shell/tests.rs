// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tests of the `shell` tool through its public `Tool` surface.

use super::ShellTool;

/// Whether process `pid` is gone; an exited but unreaped process counts (its
/// parent, or init when the parent died, reaps it).
#[cfg(target_os = "linux")]
pub(super) fn process_is_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z')),
    }
}

mod behaviour {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "shell".into(),
            args,
        }
    }

    // ── Successful execution ──────────────────────────────────────────────────

    #[tokio::test]
    async fn executes_echo_and_returns_stdout() {
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "echo hello"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hello"));
    }

    // stderr redirection syntax (>&2) is bash-specific.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_and_stderr_both_captured() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": "echo out && echo err >&2"
                }),
            ))
            .await;
        assert!(out.content.contains("out"));
        assert!(out.content.contains("err"));
    }

    #[tokio::test]
    async fn workdir_changes_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let t = ShellTool::default();
        // Use a cross-platform way to print the current directory.
        #[cfg(unix)]
        let cmd = "pwd";
        #[cfg(windows)]
        let cmd = "cd";
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": cmd,
                    "workdir": dir.path().to_str().unwrap()
                }),
            ))
            .await;
        assert!(!out.is_error);
        // Verify we're in the expected directory (path component should appear).
        let dir_name = dir.path().file_name().unwrap().to_string_lossy();
        assert!(out.content.contains(dir_name.as_ref()), "{}", out.content);
    }

    // ── Failure cases ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn exit_1_is_not_error_but_includes_code() {
        // Exit code 1 is "no matches" for grep/rg and "false" for test - not a hard error.
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "exit 1"})))
            .await;
        assert!(!out.is_error, "silent exit 1 should not set is_error");
        assert!(out.content.contains("Exit: 1"), "{}", out.content);
    }

    /// Exit 1 with a message is a failure (a traceback, a complaint), not
    /// "no matches"; only silent exit 1 is exempt.
    #[tokio::test]
    async fn exit_1_with_output_is_an_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(
                "1",
                json!({"shell_command": "echo boom >&2; exit 1"}),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("boom") && out.content.contains("Exit: 1"));
    }

    #[tokio::test]
    async fn exit_2_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "exit 2"})))
            .await;
        assert!(out.is_error, "exit code >= 2 should set is_error");
        assert!(out.content.contains("Exit: 2"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_command_argument_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("shell_command"));
    }

    /// A command matching `tools.deny_patterns` is refused and never runs,
    /// whatever the session's approval mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_matching_a_deny_pattern_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let t = ShellTool {
            policy: std::sync::Arc::new(sven_tool_registry::ToolPolicy::from_patterns(
                &[],
                &["touch *".to_string()],
            )),
            ..Default::default()
        };
        let out = t
            .execute(&call(
                "1",
                json!({ "shell_command": format!("touch {}", marker.display()) }),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("deny_patterns"), "{}", out.content);
        assert!(!marker.exists(), "the command did not run");
    }

    // `sleep` is Unix-specific; on Windows use `timeout /t N`.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_error() {
        let t = ShellTool {
            timeout_secs: 1,
            ..Default::default()
        };
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": "sleep 60",
                    "timeout_secs": 1
                }),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("timeout"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_cannot_ask_for_more_than_the_configured_ceiling() {
        let t = ShellTool {
            max_timeout_secs: 1,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let out = t
            .execute(&call(
                "1",
                json!({ "shell_command": "sleep 60", "timeout_secs": 3600 }),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.contains("timeout after 1s")
                && out.content.contains("The limit used was 1s")
                && out.content.contains("longest a call may ask for is 1s")
                && out.content.contains("Retry with a larger timeout_secs"),
            "the result names the limit that applied: {}",
            out.content
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn the_default_ceiling_is_half_an_hour_and_a_bare_call_gets_thirty_seconds() {
        let tool = ShellTool::default();
        assert_eq!(tool.max_timeout_secs, 1800);
        assert_eq!(tool.timeout_secs, 30);
        let schema = tool.parameters_schema().to_string();
        assert!(
            schema.contains("default 30") && schema.contains("max 1800"),
            "{schema}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_timed_out_command_leaves_none_of_its_processes_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        // The shell stays alive after starting `sleep`, so the sleep is a
        // grandchild that only a group kill reaches.
        let command = format!("sleep 300 & echo $! > {}; wait", pid_file.display());
        let out = ShellTool::default()
            .execute(&call(
                "1",
                json!({ "shell_command": command, "timeout_secs": 1 }),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("timeout"),
            "{}",
            out.content
        );

        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..50 {
            if super::process_is_gone(pid) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("process {pid} outlived the timed-out command");
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_asks_for_nothing_but_the_command_and_how_to_run_it() {
        let tool = ShellTool::default();
        let schema = tool.parameters_schema();
        let properties = schema["properties"].as_object().unwrap();
        assert!(
            !properties.contains_key("description"),
            "the model is asked to describe its own call"
        );
    }

    #[test]
    fn schema_has_required_command_field() {
        let t = ShellTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("shell_command")));
    }
}

mod adversarial {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "adv".into(),
            name: "shell".into(),
            args,
        }
    }

    #[tokio::test]
    async fn the_common_command_argument_name_is_accepted() {
        // Most models are trained on a shell tool whose argument is `command`;
        // a call that names it so runs instead of failing turn after turn.
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"command": "echo hello"}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hello"), "{}", out.content);
    }

    #[tokio::test]
    async fn null_command_value_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": null}))).await;
        assert!(
            out.is_error,
            "null command should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn integer_command_value_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": 42}))).await;
        assert!(
            out.is_error,
            "integer command should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn empty_command_does_not_crash() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": ""}))).await;
        // Must return without panicking; is_error is acceptable
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn very_long_command_string_does_not_crash() {
        let t = ShellTool {
            timeout_secs: 5,
            ..Default::default()
        };
        let long_cmd = format!("echo {}", "A".repeat(1_000_000));
        let out = t.execute(&call(json!({"shell_command": long_cmd}))).await;
        // Must complete without panic; output may be truncated
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn command_with_shell_metacharacters_does_not_crash() {
        let t = ShellTool {
            timeout_secs: 5,
            ..Default::default()
        };
        // The shell_command runs under sh -c; these special chars must not cause
        // a panic in the Rust layer even if sh reports an error.
        let out = t
            .execute(&call(
                json!({"shell_command": "echo 'single;quotes && ok'"}),
            ))
            .await;
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn command_producing_large_output_is_truncated_not_oom() {
        let t = ShellTool {
            timeout_secs: 10,
            ..Default::default()
        };
        // Generate ~500 KB of output; the truncation logic must keep memory bounded.
        let out = t
            .execute(&call(json!({"shell_command": "yes | head -c 512000"})))
            .await;
        assert!(
            out.content.len() <= super::super::render::DEFAULT_VISIBLE_BYTES,
            "output grew beyond expected bound: {} bytes",
            out.content.len()
        );
    }

    #[tokio::test]
    async fn workdir_pointing_to_nonexistent_path_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "pwd",
                "workdir": "sven_adversarial_nonexistent_dir_xyz"
            })))
            .await;
        assert!(
            out.is_error,
            "nonexistent workdir should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn extra_unknown_fields_are_ignored() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "echo ok",
                "totally_unknown_field": "value",
                "another_unknown": 999
            })))
            .await;
        assert!(
            !out.is_error,
            "unknown extra fields should not cause failure: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn unicode_in_command_does_not_crash() {
        let t = ShellTool::default();
        // RTL override, zero-width joiner, multi-byte sequences
        let out = t
            .execute(&call(
                json!({"shell_command": "echo '日本語 \u{202E}RTL\u{200D}ZWJ'"}),
            ))
            .await;
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn newline_in_workdir_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "pwd",
                "workdir": "line1\nline2"
            })))
            .await;
        // Paths containing newlines are not valid directory names on Linux.
        assert!(
            out.is_error,
            "newline in workdir should be an error: {}",
            out.content
        );
    }
}
