// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tests of the gate command tool: what it runs, how it ends, and what it
//! refuses.
//!
//! Swedish Embedded AB implements verifiable command execution for agent
//! workflows for its clients. If your team needs expertise in running build
//! and test commands as trusted checks then you can procure our services by
//! sending an email to info@swedishembedded.com.

use super::*;

fn tool(dir: &tempfile::TempDir) -> GateCommandTool {
    GateCommandTool::new(dir.path()).expect("tool")
}

/// Runs a call. A call written with a single `argv` is one step named `t`.
async fn run(tool: &GateCommandTool, mut args: Value) -> (bool, Value) {
    if let Some(argv) = args.as_object_mut().and_then(|a| a.remove("argv")) {
        args["steps"] = json!([{ "name": "t", "argv": argv }]);
    }
    let out = tool
        .execute(&ToolCall {
            id: "g".into(),
            name: GATE_TOOL.into(),
            args,
        })
        .await;
    let report = serde_json::from_str(&out.content).unwrap_or(Value::String(out.content));
    (out.is_error, report)
}

#[tokio::test]
async fn it_reports_the_exit_status_and_the_output_of_a_program() {
    let dir = tempfile::tempdir().unwrap();
    let (is_error, r) = run(
        &tool(&dir),
        json!({ "argv": ["sh", "-c", "echo out; echo err >&2; exit 3"], "timeout_secs": 10 }),
    )
    .await;
    assert!(!is_error, "a failing program is a report, not a tool error");
    assert_eq!(r["exit_code"], 3);
    assert_eq!(r["stdout"], "out\n");
    assert_eq!(r["stderr"], "err\n");
    assert_eq!(r["timed_out"], false);
    assert_eq!(r["error"], Value::Null);
}

#[tokio::test]
async fn the_command_reads_the_text_it_is_given_on_standard_input() {
    let dir = tempfile::tempdir().unwrap();
    // Far more than an argument can hold, and with every awkward byte in it.
    let text = format!(
        "{}{{{{ not a template }}}} $HOME `x`\n",
        "line\n".repeat(400_000)
    );
    let (is_error, r) = run(
        &tool(&dir),
        json!({ "argv": ["wc", "-c"], "timeout_secs": 20, "stdin": text }),
    )
    .await;
    assert!(!is_error);
    assert_eq!(r["exit_code"], 0);
    assert_eq!(r["stdout"].as_str().unwrap().trim(), text.len().to_string());
}

#[tokio::test]
async fn the_command_finds_the_variables_it_is_given_in_its_environment() {
    let dir = tempfile::tempdir().unwrap();
    let (is_error, r) = run(
        &tool(&dir),
        json!({
            "argv": ["sh", "-c", "printf %s \"$SVEN_SCRATCHPAD\""],
            "timeout_secs": 10,
            "env": { "SVEN_SCRATCHPAD": "/some/dir" }
        }),
    )
    .await;
    assert!(!is_error);
    assert_eq!(r["stdout"], "/some/dir");
    for bad in [json!({ "A-B": "x" }), json!({ "A": 1 }), json!("x")] {
        let (is_error, _) = run(
            &tool(&dir),
            json!({ "argv": ["true"], "timeout_secs": 10, "env": bad }),
        )
        .await;
        assert!(is_error, "{bad} is refused");
    }
}

fn named(steps: &[(&str, &str)]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|(name, script)| json!({ "name": name, "argv": ["sh", "-c", script] }))
            .collect(),
    )
}

#[tokio::test]
async fn steps_run_in_order_and_the_first_failure_ends_the_gate_and_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let steps = named(&[
        ("one", "echo 1 >> log"),
        ("two", "echo oops >&2; exit 4"),
        ("three", "echo 3 >> log"),
    ]);
    let (is_error, r) = run(&tool(&dir), json!({ "steps": steps, "timeout_secs": 10 })).await;
    assert!(!is_error);
    assert_eq!(r["failed_step"], "two");
    assert_eq!(r["exit_code"], 4);
    assert_eq!(r["stderr"], "oops\n", "the report is the failing step's");
    let ran: Vec<_> = r["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(ran, [json!("one"), json!("two")], "the third never started");
    assert_eq!(r["steps"][0]["exit_code"], 0);
    assert!(r["steps"][1]["duration_ms"].is_u64());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("log")).unwrap(),
        "1\n"
    );
}

#[tokio::test]
async fn a_gate_whose_steps_all_pass_reports_the_last_and_no_failure() {
    let dir = tempfile::tempdir().unwrap();
    let steps = named(&[("a", "echo first"), ("b", "echo second")]);
    let (_, r) = run(&tool(&dir), json!({ "steps": steps, "timeout_secs": 10 })).await;
    assert_eq!(r["failed_step"], Value::Null);
    assert_eq!(r["exit_code"], 0);
    assert_eq!(r["stdout"], "second\n");
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn one_deadline_bounds_all_the_steps_together() {
    let dir = tempfile::tempdir().unwrap();
    let steps = named(&[("fast", "sleep 1"), ("slow", "sleep 30"), ("never", "true")]);
    let started = Instant::now();
    let (_, r) = run(&tool(&dir), json!({ "steps": steps, "timeout_secs": 2 })).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(r["timed_out"], true);
    assert_eq!(r["failed_step"], "slow");
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn every_step_is_given_the_same_standard_input() {
    let dir = tempfile::tempdir().unwrap();
    let steps = named(&[("a", "cat >> got"), ("b", "cat >> got")]);
    let (_, r) = run(
        &tool(&dir),
        json!({ "steps": steps, "timeout_secs": 10, "stdin": "x" }),
    )
    .await;
    assert_eq!(r["exit_code"], 0, "{r}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("got")).unwrap(),
        "xx"
    );
}

#[tokio::test]
async fn without_stdin_the_command_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = run(
        &tool(&dir),
        json!({ "argv": ["wc", "-c"], "timeout_secs": 10 }),
    )
    .await;
    assert_eq!(r["stdout"].as_str().unwrap().trim(), "0");
}

#[tokio::test]
async fn a_command_that_ignores_its_input_does_not_hang_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(8 * 1024 * 1024);
    let (_, r) = run(
        &tool(&dir),
        json!({ "argv": ["true"], "timeout_secs": 20, "stdin": big }),
    )
    .await;
    assert_eq!(
        r["exit_code"], 0,
        "a closed pipe is not a failure of the gate: {r}"
    );
}

#[tokio::test]
async fn success_is_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = run(&tool(&dir), json!({ "argv": ["true"], "timeout_secs": 10 })).await;
    assert_eq!(r["exit_code"], 0);
}

#[tokio::test]
async fn there_is_no_shell_to_interpolate_into() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = run(
        &tool(&dir),
        json!({ "argv": ["echo", "$HOME; touch pwned"], "timeout_secs": 10 }),
    )
    .await;
    assert_eq!(r["stdout"], "$HOME; touch pwned\n");
    assert!(!dir.path().join("pwned").exists());
}

#[tokio::test]
async fn a_program_that_cannot_start_is_reported_as_an_error_without_a_status() {
    let dir = tempfile::tempdir().unwrap();
    let (is_error, r) = run(
        &tool(&dir),
        json!({ "argv": ["/nonexistent/program"], "timeout_secs": 10 }),
    )
    .await;
    assert!(!is_error);
    assert_eq!(r["exit_code"], Value::Null);
    assert!(r["error"]
        .as_str()
        .is_some_and(|e| e.contains("/nonexistent/program")));
}

#[tokio::test]
async fn a_program_out_of_time_is_killed_with_everything_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("survivor");
    let script = format!("(sleep 3; touch {}) & wait", marker.display());
    let started = Instant::now();
    let (_, r) = run(
        &tool(&dir),
        json!({ "argv": ["sh", "-c", script], "timeout_secs": 1 }),
    )
    .await;
    assert_eq!(r["timed_out"], true);
    assert_eq!(r["exit_code"], Value::Null);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "returned at the deadline"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!marker.exists(), "the descendant was killed with the group");
}

#[tokio::test]
async fn only_the_tail_of_long_output_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (_, r) = run(
        &tool(&dir),
        json!({ "argv": ["sh", "-c", "yes line | head -c 200000; echo END"], "timeout_secs": 10 }),
    )
    .await;
    let out = r["stdout"].as_str().unwrap();
    assert!(out.len() <= STREAM_TAIL_BYTES);
    assert!(out.ends_with("END\n"));
    assert_eq!(r["truncated"], true);
}

#[tokio::test]
async fn the_working_directory_stays_inside_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let tool = tool(&dir);
    let (is_error, r) = run(
        &tool,
        json!({ "argv": ["pwd"], "timeout_secs": 5, "cwd": "sub" }),
    )
    .await;
    assert!(!is_error);
    assert!(r["stdout"].as_str().unwrap().trim_end().ends_with("/sub"));
    for escape in ["..", "/", "sub/../../.."] {
        let (is_error, _) = run(
            &tool,
            json!({ "argv": ["pwd"], "timeout_secs": 5, "cwd": escape }),
        )
        .await;
        assert!(is_error, "cwd `{escape}` must be refused");
    }
}

#[tokio::test]
async fn malformed_calls_are_tool_errors() {
    let dir = tempfile::tempdir().unwrap();
    let tool = tool(&dir);
    for args in [
        json!({ "timeout_secs": 5 }),
        json!({ "steps": [], "timeout_secs": 5 }),
        json!({ "steps": [{ "argv": ["true"] }], "timeout_secs": 5 }),
        json!({ "steps": [{ "name": "", "argv": ["true"] }], "timeout_secs": 5 }),
        json!({ "argv": [], "timeout_secs": 5 }),
        json!({ "argv": [""], "timeout_secs": 5 }),
        json!({ "argv": [1], "timeout_secs": 5 }),
        json!({ "argv": ["true"] }),
    ] {
        let (is_error, _) = run(&tool, args.clone()).await;
        assert!(is_error, "{args}");
    }
}

#[test]
fn no_agent_mode_lists_the_tool_and_it_needs_the_shell_capability() {
    let dir = tempfile::tempdir().unwrap();
    let tool = tool(&dir);
    assert!(tool.modes().is_empty());
    assert_eq!(tool.kernel_capability(), ToolCapability::ExecuteShell);
    assert_eq!(tool.name(), "shell_gate");
}

#[tokio::test]
async fn a_trusted_directory_outside_the_workspace_is_where_the_command_runs() {
    let workspace = tempfile::tempdir().unwrap();
    let trusted = tempfile::tempdir().unwrap();
    std::fs::write(trusted.path().join("pinned.txt"), "x").unwrap();
    let (is_error, r) = run(
        &tool(&workspace),
        json!({ "argv": ["ls"], "timeout_secs": 10,
                "trusted_cwd": trusted.path().to_str().unwrap() }),
    )
    .await;
    assert!(!is_error, "{r}");
    assert_eq!(r["stdout"], "pinned.txt\n");
}

#[tokio::test]
async fn a_trusted_directory_the_agent_can_write_is_refused() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("inside")).unwrap();
    let parent = workspace
        .path()
        .parent()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    for bad in [
        workspace
            .path()
            .join("inside")
            .to_str()
            .unwrap()
            .to_string(),
        workspace.path().to_str().unwrap().to_string(),
        parent,
        "relative/dir".to_string(),
        "/no/such/dir".to_string(),
    ] {
        let (is_error, r) = run(
            &tool(&workspace),
            json!({ "argv": ["true"], "timeout_secs": 10, "trusted_cwd": bad }),
        )
        .await;
        assert!(is_error, "{bad} must be refused: {r}");
    }
    // A symlink in the workspace to a directory outside is still outside,
    // but one to a directory inside is inside.
    let link = workspace.path().join("link");
    std::os::unix::fs::symlink(workspace.path().join("inside"), &link).unwrap();
    let (is_error, _) = run(
        &tool(&workspace),
        json!({ "argv": ["true"], "timeout_secs": 10, "trusted_cwd": link.to_str().unwrap() }),
    )
    .await;
    assert!(is_error);
}

#[tokio::test]
async fn the_trajectory_is_written_before_the_command_and_a_failure_to_write_fails_closed() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("trajectory.json");
    let hook: TrajectoryHook = std::sync::Arc::new(|path| {
        std::fs::write(path, "{\"steps\": []}").map_err(|e| e.to_string())
    });
    let with = GateCommandTool::new(workspace.path())
        .unwrap()
        .with_trajectory_hook(hook);
    let (is_error, r) = run(
        &with,
        json!({ "argv": ["cat", file.to_str().unwrap()], "timeout_secs": 10,
                "trajectory_file": file.to_str().unwrap() }),
    )
    .await;
    assert!(!is_error, "{r}");
    assert_eq!(r["stdout"], "{\"steps\": []}");
    // No hook: the host cannot provide it, so the gate does not run.
    let (is_error, _) = run(
        &tool(&workspace),
        json!({ "argv": ["true"], "timeout_secs": 10, "trajectory_file": "/x" }),
    )
    .await;
    assert!(is_error);
}

#[tokio::test]
async fn a_gate_may_keep_more_of_its_output_within_a_limit() {
    let dir = tempfile::tempdir().unwrap();
    let ask = |bytes: Value| {
        json!({ "argv": ["sh", "-c", "head -c 60000 /dev/zero | tr '\\0' x"],
                "timeout_secs": 20, "tail_bytes": bytes })
    };
    let (_, small) = run(
        &tool(&dir),
        json!({ "argv": ["sh", "-c", "head -c 60000 /dev/zero | tr '\\0' x"], "timeout_secs": 20 }),
    )
    .await;
    assert_eq!(small["stdout"].as_str().unwrap().len(), STREAM_TAIL_BYTES);
    let (_, big) = run(&tool(&dir), ask(json!(65536))).await;
    assert_eq!(big["stdout"].as_str().unwrap().len(), 60000);
    assert_eq!(big["truncated"], false);
    for bad in [json!(10), json!(2_000_000), json!("lots")] {
        let (is_error, _) = run(&tool(&dir), ask(bad)).await;
        assert!(is_error);
    }
}
